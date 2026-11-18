mod decode;
mod keymap;
mod net;
mod render;

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use moqdesk_proto::{CursorUpdate, DEFAULT_DESK, InputEvent, Ticket, button};
use tokio::sync::mpsc::UnboundedSender;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{CustomCursor, Window, WindowId};

use crate::decode::Image;
use crate::render::{CursorShape, Viewport};

/// View and control a moqdesk host.
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
	/// Ticket printed by moqdesk-host (moqdesk://HOST:PORT/TOKEN#FINGERPRINT).
	ticket: Option<String>,

	/// Connect to a relay URL instead of using a ticket, e.g. https://relay.example.com/team.
	#[arg(long, conflicts_with = "ticket")]
	url: Option<url::Url>,

	/// Broadcast path of the desktop.
	#[arg(long, default_value = DEFAULT_DESK)]
	name: String,

	/// Pin the server certificate by SHA-256 fingerprint (hex).
	#[arg(long)]
	fingerprint: Option<String>,

	/// Disable TLS verification (testing only).
	#[arg(long)]
	insecure: bool,

	/// End-to-end key printed by moqdesk-host in relay mode.
	#[arg(long, env = "MOQDESK_KEY", hide_env_values = true)]
	key: Option<String>,

	/// Don't send any input.
	#[arg(long)]
	view_only: bool,

	/// Headless: save a PNG of the remote desktop and exit.
	#[arg(long)]
	snapshot: Option<PathBuf>,

	/// Headless: click at X,Y (remote pixels) after the first frame.
	#[arg(long, value_parser = parse_point)]
	click: Option<(u16, u16)>,

	/// Headless: type this text (US layout) after the first frame.
	#[arg(long = "type")]
	text: Option<String>,

	/// Headless: seconds to keep watching after sending input, before the snapshot.
	#[arg(long, default_value_t = 1.0)]
	wait: f32,

	/// Headless: give up if no frame arrives within this many seconds.
	#[arg(long, default_value_t = 20.0)]
	timeout: f32,
}

fn parse_point(s: &str) -> Result<(u16, u16), String> {
	let (x, y) = s.split_once(',').ok_or("expected X,Y")?;
	Ok((
		x.trim().parse().map_err(|_| "bad X")?,
		y.trim().parse().map_err(|_| "bad Y")?,
	))
}

#[derive(Default)]
struct Shared {
	frame: Mutex<Option<Image>>,
	cursor: Mutex<CursorState>,
	frames: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct CursorState {
	shape: Option<Arc<CursorShape>>,
	serial: u64,
	x: i32,
	y: i32,
}

impl Shared {
	fn apply_cursor(&self, update: CursorUpdate) {
		let mut cursor = self.cursor.lock().unwrap();
		match update {
			CursorUpdate::Shape {
				x,
				y,
				width,
				height,
				hot_x,
				hot_y,
				rgba,
			} => {
				if rgba.len() == width as usize * height as usize * 4 {
					cursor.shape = Some(Arc::new(CursorShape {
						width: width as usize,
						height: height as usize,
						hot_x: hot_x as i32,
						hot_y: hot_y as i32,
						rgba,
					}));
					cursor.serial += 1;
				}
				(cursor.x, cursor.y) = (x as i32, y as i32);
			}
			CursorUpdate::Position { x, y } => (cursor.x, cursor.y) = (x as i32, y as i32),
		}
	}

	/// The latest frame with the remote cursor drawn in.
	fn composited(&self) -> Option<Image> {
		let mut image = self.frame.lock().unwrap().clone()?;
		let cursor = self.cursor.lock().unwrap();
		if let Some(shape) = &cursor.shape {
			render::composite(&mut image, shape, cursor.x, cursor.y);
		}
		Some(image)
	}
}

#[derive(Debug)]
enum Wake {
	Frame,
	Cursor,
	Status(String),
	Exit(String),
}

fn main() -> Result<()> {
	moq_tokio::Log::new(tracing::Level::INFO).init()?;
	let args = Args::parse();

	let key = args.key.as_deref().map(moqdesk_proto::e2e::Cipher::parse).transpose()?;
	let target = match (&args.ticket, &args.url) {
		(Some(ticket), _) => {
			let ticket = Ticket::parse(ticket)?;
			net::Target {
				url: ticket.url().parse()?,
				name: args.name.clone(),
				fingerprint: Some(ticket.fingerprint),
				insecure: args.insecure,
				key: key.clone(),
			}
		}
		(None, Some(url)) => net::Target {
			url: url.clone(),
			name: args.name.clone(),
			fingerprint: args.fingerprint.clone(),
			insecure: args.insecure,
			key: key.clone(),
		},
		(None, None) => bail!("pass a ticket from moqdesk-host, or --url for a relay"),
	};

	let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
	let shared = Arc::new(Shared::default());
	let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
	let (packet_tx, packet_rx) = mpsc::channel();

	let headless = args.snapshot.is_some() || args.click.is_some() || args.text.is_some();
	let event_loop = if headless {
		None
	} else {
		Some(EventLoop::<Wake>::with_user_event().build()?)
	};
	let proxy = event_loop.as_ref().map(|e| e.create_proxy());
	let wake = {
		let proxy = proxy.clone();
		move |w: Wake| {
			if let Some(p) = &proxy {
				let _ = p.send_event(w);
			}
		}
	};

	std::thread::Builder::new().name("decode".into()).spawn({
		let (shared, wake) = (shared.clone(), wake.clone());
		move || {
			decode::run(packet_rx, |image| {
				*shared.frame.lock().unwrap() = Some(image);
				shared.frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				wake(Wake::Frame);
			})
		}
	})?;

	runtime.spawn({
		let (shared, wake) = (shared.clone(), wake.clone());
		async move {
			let updates = move |u: net::Update| match u {
				net::Update::Cursor(update) => {
					shared.apply_cursor(update);
					wake(Wake::Cursor);
				}
				net::Update::Status(s) => {
					tracing::info!("{s}");
					wake(Wake::Status(s));
				}
			};
			let res = net::run(target, input_rx, packet_tx, updates).await;
			let msg = match res {
				Ok(()) => "disconnected".to_string(),
				Err(err) => format!("{err:#}"),
			};
			tracing::error!("{msg}");
			if let Some(p) = proxy {
				let _ = p.send_event(Wake::Exit(msg));
			} else {
				std::process::exit(1);
			}
		}
	});

	let input = (!args.view_only).then_some(input_tx);
	if let Some(input) = &input {
		let _ = input.send(InputEvent::RequestKeyframe);
	}

	match event_loop {
		None => runtime.block_on(headless_run(&args, &shared, input)),
		Some(event_loop) => {
			let mut app = App::new(shared, input);
			event_loop.run_app(&mut app)?;
			let exit = app.exit.take();
			// Dropping the sender finishes the input track; give it a moment to reach the host.
			drop(app);
			runtime.block_on(async { tokio::time::sleep(Duration::from_millis(300)).await });
			exit.map_or(Ok(()), |e| Err(anyhow::anyhow!(e)))
		}
	}
}

async fn headless_run(args: &Args, shared: &Shared, input: Option<UnboundedSender<InputEvent>>) -> Result<()> {
	let deadline = Instant::now() + Duration::from_secs_f32(args.timeout);
	while shared.frame.lock().unwrap().is_none() {
		if Instant::now() > deadline {
			bail!("no video within {}s", args.timeout);
		}
		tokio::time::sleep(Duration::from_millis(20)).await;
	}
	let send = |e: InputEvent| {
		if let Some(input) = &input {
			let _ = input.send(e);
		}
	};
	if let Some((x, y)) = args.click {
		send(InputEvent::PointerMove { x, y });
		send(InputEvent::Button {
			button: button::LEFT,
			pressed: true,
		});
		send(InputEvent::Button {
			button: button::LEFT,
			pressed: false,
		});
		tokio::time::sleep(Duration::from_millis(200)).await;
	}
	if let Some(text) = &args.text {
		for c in text.chars() {
			let Some((hid, shift)) = moqdesk_proto::ascii_to_hid(c) else {
				tracing::warn!(?c, "can't type character");
				continue;
			};
			if shift {
				send(InputEvent::Key {
					hid: moqdesk_proto::HID_LEFT_SHIFT,
					pressed: true,
				});
			}
			send(InputEvent::Key { hid, pressed: true });
			send(InputEvent::Key { hid, pressed: false });
			if shift {
				send(InputEvent::Key {
					hid: moqdesk_proto::HID_LEFT_SHIFT,
					pressed: false,
				});
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	}
	tokio::time::sleep(Duration::from_secs_f32(args.wait)).await;
	let frames = shared.frames.load(std::sync::atomic::Ordering::Relaxed);
	let image = shared.composited().context("no frame")?;
	if let Some(path) = &args.snapshot {
		render::save_png(&image, path)?;
		println!(
			"saved {}x{} snapshot to {} ({frames} frames decoded)",
			image.width,
			image.height,
			path.display()
		);
	}
	Ok(())
}

struct App {
	shared: Arc<Shared>,
	input: Option<UnboundedSender<InputEvent>>,
	window: Option<Rc<Window>>,
	surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
	viewport: Option<Viewport>,
	pointer_inside: bool,
	cursor_serial: u64,
	scroll: (f64, f64),
	status: String,
	stats: (Instant, u64),
	exit: Option<String>,
}

impl App {
	fn new(shared: Arc<Shared>, input: Option<UnboundedSender<InputEvent>>) -> Self {
		Self {
			shared,
			input,
			window: None,
			surface: None,
			viewport: None,
			pointer_inside: false,
			cursor_serial: 0,
			scroll: (0.0, 0.0),
			status: "connecting".into(),
			stats: (Instant::now(), 0),
			exit: None,
		}
	}

	fn send(&self, event: InputEvent) {
		if let Some(input) = &self.input {
			let _ = input.send(event);
		}
	}

	fn redraw(&mut self) -> Result<()> {
		let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
			return Ok(());
		};
		let size = window.inner_size();
		let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
			return Ok(());
		};
		surface.resize(w, h).map_err(|e| anyhow::anyhow!("{e}"))?;
		let mut buffer = surface.buffer_mut().map_err(|e| anyhow::anyhow!("{e}"))?;
		// While the local pointer is over the window the OS draws the remote cursor shape
		// with zero latency; otherwise draw it into the frame at the remote position.
		let image = if self.pointer_inside && self.input.is_some() {
			self.shared.frame.lock().unwrap().clone()
		} else {
			self.shared.composited()
		};
		match image {
			Some(image) => {
				self.viewport = Some(render::blit(&image, &mut buffer, (w.get() as usize, h.get() as usize)))
			}
			None => buffer.fill(0x202020),
		}
		buffer.present().map_err(|e| anyhow::anyhow!("{e}"))?;
		Ok(())
	}

	fn update_title(&mut self) {
		let Some(window) = &self.window else { return };
		let frames = self.shared.frames.load(std::sync::atomic::Ordering::Relaxed);
		let elapsed = self.stats.0.elapsed().as_secs_f64();
		if elapsed < 1.0 {
			return;
		}
		let fps = (frames - self.stats.1) as f64 / elapsed;
		self.stats = (Instant::now(), frames);
		let size = self
			.viewport
			.map(|v| format!(" {}x{}", v.remote.0, v.remote.1))
			.unwrap_or_default();
		window.set_title(&format!("moqdesk — {}{size} {fps:.0} fps", self.status));
	}

	fn update_cursor(&mut self, event_loop: &ActiveEventLoop) {
		let Some(window) = &self.window else { return };
		let cursor = self.shared.cursor.lock().unwrap();
		if cursor.serial == self.cursor_serial {
			return;
		}
		self.cursor_serial = cursor.serial;
		let Some(shape) = &cursor.shape else { return };
		if shape.width == 0 || shape.height == 0 {
			return;
		}
		match CustomCursor::from_rgba(
			shape.rgba.clone(),
			shape.width as u16,
			shape.height as u16,
			shape.hot_x as u16,
			shape.hot_y as u16,
		) {
			Ok(source) => window.set_cursor(event_loop.create_custom_cursor(source)),
			Err(err) => tracing::debug!(%err, "unusable cursor shape"),
		}
	}
}

impl ApplicationHandler<Wake> for App {
	fn resumed(&mut self, event_loop: &ActiveEventLoop) {
		if self.window.is_some() {
			return;
		}
		let attrs = Window::default_attributes()
			.with_title("moqdesk")
			.with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0));
		let result = (|| -> Result<()> {
			let window = Rc::new(event_loop.create_window(attrs)?);
			let context = softbuffer::Context::new(window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;
			self.surface =
				Some(softbuffer::Surface::new(&context, window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?);
			self.window = Some(window);
			Ok(())
		})();
		if let Err(err) = result {
			self.exit = Some(format!("failed to open window: {err:#}"));
			event_loop.exit();
		}
	}

	fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Wake) {
		match event {
			Wake::Frame => {
				if let Some(w) = &self.window {
					w.request_redraw();
				}
			}
			Wake::Cursor => {
				self.update_cursor(event_loop);
				if !self.pointer_inside
					&& let Some(w) = &self.window
				{
					w.request_redraw();
				}
			}
			Wake::Status(status) => {
				self.status = status;
				self.stats.0 = Instant::now() - Duration::from_secs(1);
				self.update_title();
			}
			Wake::Exit(msg) => {
				self.exit = Some(msg);
				event_loop.exit();
			}
		}
	}

	fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
		match event {
			WindowEvent::CloseRequested => {
				// Closing via a shortcut (e.g. Alt+F4) never delivers the modifier's release.
				self.send(InputEvent::ReleaseAll);
				event_loop.exit();
			}
			WindowEvent::RedrawRequested => {
				if let Err(err) = self.redraw() {
					tracing::warn!("render failed: {err:#}");
				}
				self.update_title();
			}
			WindowEvent::Resized(_) => {
				if let Some(w) = &self.window {
					w.request_redraw();
				}
			}
			WindowEvent::CursorEntered { .. } => {
				self.pointer_inside = true;
				self.window.as_ref().inspect(|w| w.request_redraw());
			}
			WindowEvent::CursorLeft { .. } => {
				self.pointer_inside = false;
				self.window.as_ref().inspect(|w| w.request_redraw());
			}
			WindowEvent::CursorMoved { position, .. } => {
				if let Some(vp) = self.viewport {
					let (x, y) = vp.remote_point(position.x, position.y);
					self.send(InputEvent::PointerMove { x, y });
				}
			}
			WindowEvent::MouseInput { state, button, .. } => {
				let button = match button {
					MouseButton::Left => button::LEFT,
					MouseButton::Middle => button::MIDDLE,
					MouseButton::Right => button::RIGHT,
					MouseButton::Back => button::BACK,
					MouseButton::Forward => button::FORWARD,
					MouseButton::Other(_) => return,
				};
				self.send(InputEvent::Button {
					button,
					pressed: state == ElementState::Pressed,
				});
			}
			WindowEvent::MouseWheel { delta, .. } => {
				let (dx, dy) = match delta {
					MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64),
					MouseScrollDelta::PixelDelta(p) => (p.x / 60.0, p.y / 60.0),
				};
				self.scroll.0 += dx;
				self.scroll.1 += dy;
				let (nx, ny) = (self.scroll.0.trunc(), self.scroll.1.trunc());
				if nx != 0.0 || ny != 0.0 {
					self.scroll.0 -= nx;
					self.scroll.1 -= ny;
					// winit reports positive y for scrolling up; the protocol uses positive for down.
					self.send(InputEvent::Scroll {
						dx: nx as i16,
						dy: -ny as i16,
					});
				}
			}
			WindowEvent::KeyboardInput { event, .. } => {
				// The host's X server generates its own autorepeat.
				if event.repeat {
					return;
				}
				if let PhysicalKey::Code(code) = event.physical_key
					&& let Some(hid) = keymap::keycode_to_hid(code)
				{
					self.send(InputEvent::Key {
						hid,
						pressed: event.state == ElementState::Pressed,
					});
				}
			}
			WindowEvent::Focused(false) => self.send(InputEvent::ReleaseAll),
			_ => {}
		}
	}
}
