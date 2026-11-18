mod capture;
mod clock;
mod cursor;
mod encode;
mod inject;
mod video;

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use moqdesk_proto::e2e::{Cipher, Sealer};
use moqdesk_proto::{CURSOR_TRACK, DEFAULT_DESK, INPUT_TRACK, InputEvent, Ticket, VIDEO_TRACK, VIEWER_PREFIX};

use crate::clock::Clock;
use crate::inject::Command;

/// Share this X11 desktop over Media over QUIC.
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
	/// Address for the embedded MoQ server (direct mode).
	#[arg(long, default_value = "[::]:4443")]
	listen: SocketAddr,

	/// Publish through a MoQ relay instead of serving directly, e.g. https://relay.example.com/team.
	#[arg(long)]
	relay: Option<url::Url>,

	/// Disable TLS verification when connecting to the relay (testing only).
	#[arg(long, requires = "relay")]
	relay_insecure: bool,

	/// Broadcast path of this desktop.
	#[arg(long, default_value = DEFAULT_DESK)]
	name: String,

	/// X display to capture (defaults to $DISPLAY).
	#[arg(long)]
	display: Option<String>,

	/// Maximum frames per second.
	#[arg(long, default_value_t = 30.0)]
	fps: f32,

	/// Target video bitrate in bits per second.
	#[arg(long, default_value_t = 6_000_000)]
	bitrate: u32,

	/// Seconds between forced keyframes (new MoQ groups).
	#[arg(long, default_value_t = 5.0)]
	gop: f32,

	/// Ignore input from viewers.
	#[arg(long)]
	view_only: bool,

	/// End-to-end encrypt video, cursor and input (default: on when publishing via a relay with input enabled).
	#[arg(long, overrides_with = "no_e2e")]
	e2e: bool,

	/// Publish in plaintext through a relay, e.g. for moq.watch. Requires --view-only.
	#[arg(long, requires = "view_only")]
	no_e2e: bool,

	/// Base URL of the browser viewer printed in relay links.
	#[arg(long, default_value = "https://kareemn.github.io/moqdesk/")]
	web_viewer: url::Url,

	/// Access token viewers must present. Randomly generated if omitted.
	#[arg(long, env = "MOQDESK_TOKEN", hide_env_values = true)]
	token: Option<String>,

	/// Host name or IP to put in the printed ticket (defaults to the primary local IP).
	#[arg(long)]
	public_host: Option<String>,

	/// Also write the ticket to this file.
	#[arg(long)]
	ticket_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
	moq_tokio::Log::new(tracing::Level::INFO).init()?;
	let args = Args::parse();
	let display = args.display.clone();

	let clock = Clock::new();
	let keyframe = Arc::new(AtomicBool::new(false));

	let publish = moq_tokio::origin::spawn();
	let broadcast = publish
		.create_broadcast(&args.name)
		.context("failed to create broadcast")?;
	broadcast
		.announce(Default::default())
		.context("failed to announce broadcast")?;

	let cipher = (args.e2e || (args.relay.is_some() && !args.no_e2e)).then(|| match std::env::var("MOQDESK_KEY") {
		Ok(key) => Cipher::parse(&key).context("bad MOQDESK_KEY"),
		Err(_) => Ok(Cipher::generate()),
	});
	let cipher = cipher.transpose()?;
	let seal = |track: &str| cipher.as_ref().map(|c| Sealer::new(c, &args.name, track));
	let tracks = video::Tracks {
		seal: seal(VIDEO_TRACK),
		catalog: broadcast.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())?,
		video: broadcast.create_track(VIDEO_TRACK, hang::container::track_info(hang::catalog::PRIORITY.video))?,
	};
	let cursor_info = moq_net::track::Info::default()
		.with_timescale(moq_net::Timescale::MICRO)
		.with_priority(u8::MAX)
		.with_max_age(Duration::from_secs(1));
	let cursor_track = broadcast.create_track(CURSOR_TRACK, cursor_info)?;

	let capture = capture::Capture::new(display.as_deref())?;
	tracing::info!(size = ?capture.size(), "capturing");
	let settings = video::Settings {
		fps: args.fps,
		bitrate: args.bitrate,
		gop: Duration::from_secs_f32(args.gop),
	};
	spawn_thread("video", {
		let keyframe = keyframe.clone();
		move || video::run(capture, tracks, settings, keyframe, clock)
	});
	spawn_thread("cursor", {
		let display = display.clone();
		{
			let seal = seal(CURSOR_TRACK);
			move || cursor::run(display.as_deref(), cursor_track, clock, seal)
		}
	});

	let input = moq_tokio::origin::spawn();
	if !args.view_only {
		let injector = inject::Injector::new(display.as_deref())?;
		let (tx, rx) = mpsc::channel();
		spawn_thread("input", move || injector.run(rx));
		tokio::spawn(watch_viewers(
			input.consume(),
			args.name.clone(),
			cipher.clone(),
			tx,
			keyframe.clone(),
		));
	}

	let input = (!args.view_only).then_some(input);
	match args.relay.clone() {
		Some(url) => {
			print_relay_links(&args, &url, cipher.as_ref());
			run_relay(url, args.relay_insecure, &publish, input).await
		}
		None => run_direct(&args, &publish, input, cipher.as_ref()).await,
	}
}

fn spawn_thread(name: &str, f: impl FnOnce() -> Result<()> + Send + 'static) {
	let name = name.to_string();
	std::thread::Builder::new()
		.name(name.clone())
		.spawn(move || {
			if let Err(err) = f() {
				tracing::error!(thread = name, "fatal: {err:#}");
				std::process::exit(1);
			}
		})
		.expect("failed to spawn thread");
}

async fn run_direct(
	args: &Args,
	publish: &moq_net::origin::Producer,
	input: Option<moq_net::origin::Producer>,
	cipher: Option<&Cipher>,
) -> Result<()> {
	let host = args.public_host.clone().unwrap_or_else(|| match args.listen.ip() {
		ip if ip.is_unspecified() => primary_ip(),
		ip => ip.to_string(),
	});
	let token = args.token.clone().unwrap_or_else(random_token);

	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some(moq_tokio::listen::Bind::Addr(args.listen));
	config.tls.generate = vec![host.clone(), "localhost".into()];
	let mut server = config.init(Default::default())?.listen().await?;

	let port = server.local_addr()?.port();
	let fingerprint = server
		.certificates()
		.fingerprints()
		.into_iter()
		.next()
		.context("no certificate")?;
	let addr = if host.contains(':') {
		format!("[{host}]:{port}")
	} else {
		format!("{host}:{port}")
	};
	let ticket = Ticket {
		addr,
		token: token.clone(),
		fingerprint,
	};
	let name_arg = if args.name == DEFAULT_DESK {
		String::new()
	} else {
		format!(" --name {}", sh(&args.name))
	};
	println!(
		"\nShare this ticket with viewers:\n\n  {}moqdesk-view {}{name_arg}\n",
		key_env(cipher),
		sh(&ticket.to_string())
	);
	if let Some(path) = &args.ticket_file {
		write_private(path, &format!("{ticket}\n")).context("failed to write ticket file")?;
	}

	while let Some(request) = server.accept().await {
		let publish = publish.clone();
		let input = input.clone();
		let token = token.clone();
		tokio::spawn(async move {
			let path = request.path().trim_matches('/').to_string();
			if !constant_time_eq(path.as_bytes(), token.as_bytes()) {
				tracing::warn!("rejected viewer with a bad token");
				let _ = request.reject(moq_tokio::server::Reject::Unauthorized).await;
				return;
			}
			let mut request = request.with_publisher(&publish);
			if let Some(input) = input {
				request = request.with_subscriber(input);
			}
			match request.ok().await {
				Ok(session) => {
					tracing::info!("viewer connected");
					let res = session.closed().await;
					tracing::info!(?res, "viewer disconnected");
				}
				Err(err) => tracing::warn!(%err, "handshake failed"),
			}
		});
	}
	Ok(())
}

fn print_relay_links(args: &Args, relay: &url::Url, cipher: Option<&Cipher>) {
	let mut params = url::form_urlencoded::Serializer::new(String::new());
	params
		.append_pair("relay", relay.as_str())
		.append_pair("name", &args.name);
	let mut native = format!(
		"{}moqdesk-view --url {} --name {}",
		key_env(cipher),
		sh(relay.as_str()),
		sh(&args.name)
	);
	if args.relay_insecure {
		native += " --insecure";
	}
	if let Some(cipher) = cipher {
		params.append_pair("key", &cipher.encoded_key());
	}
	if args.view_only {
		params.append_pair("view", "1");
	}
	let mut link = args.web_viewer.clone();
	link.set_fragment(Some(&params.finish()));
	println!(
		"\nOpen in a browser (the key after # never leaves the browser):\n\n  {link}\n\nor natively:\n\n  {native}\n"
	);
}

/// Passed through the environment so the key stays out of `ps` output.
fn key_env(cipher: Option<&Cipher>) -> String {
	cipher.map_or_else(String::new, |c| format!("MOQDESK_KEY={} ", c.encoded_key()))
}

/// Quote `s` for a POSIX shell when it isn't a plain word.
fn sh(s: &str) -> String {
	let plain = |c: char| c.is_ascii_alphanumeric() || "-_./:@%+=,".contains(c);
	if !s.is_empty() && s.chars().all(plain) {
		s.to_owned()
	} else {
		format!("'{}'", s.replace('\'', r"'\''"))
	}
}

async fn run_relay(
	url: url::Url,
	insecure: bool,
	publish: &moq_net::origin::Producer,
	input: Option<moq_net::origin::Producer>,
) -> Result<()> {
	let mut config = moq_tokio::connect::Config::default();
	if insecure {
		config.tls.insecure = Some(true);
	}
	let mut client = config.init(Default::default())?.with_publisher(publish);
	if let Some(input) = input {
		client = client.with_subscriber(input);
	}
	tracing::info!(%url, "publishing via relay");
	client.connect(url).closed().await?;
	Ok(())
}

/// Subscribe to the input track of every viewer that announces itself.
async fn watch_viewers(
	origin: moq_net::origin::Consumer,
	desk: String,
	cipher: Option<Cipher>,
	tx: mpsc::Sender<Command>,
	keyframe: Arc<AtomicBool>,
) {
	// Viewers publish under `viewer/<desk>/<id>`, so input meant for another desktop on a shared relay is ignored.
	let prefix = format!("{VIEWER_PREFIX}/{desk}/");
	static NEXT_ID: AtomicU64 = AtomicU64::new(1);
	let active = Arc::new(Mutex::new(HashSet::new()));
	// Outlives each subscription, so frames from an earlier subscription to the same path can't be replayed.
	let counters = Arc::new(Mutex::new(HashMap::new()));
	let mut announced = origin.announced();
	while let Some(update) = announced.next().await {
		let path = update.prefix.to_string();
		if !update.kind.is_active() || !path.starts_with(&prefix) {
			continue;
		}
		if !active.lock().unwrap().insert(path.clone()) {
			continue;
		}
		let (origin, tx, keyframe, active, cipher, counters) = (
			origin.clone(),
			tx.clone(),
			keyframe.clone(),
			active.clone(),
			cipher.clone(),
			counters.clone(),
		);
		tokio::spawn(async move {
			let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
			tracing::info!(viewer = %path, "receiving input");
			if let Err(err) = read_input(&origin, &path, id, cipher.as_ref(), &counters, &tx, &keyframe).await {
				tracing::debug!(viewer = %path, %err, "input track ended");
			}
			let _ = tx.send(Command::ViewerGone(id));
			active.lock().unwrap().remove(&path);
		});
	}
}

async fn read_input(
	origin: &moq_net::origin::Consumer,
	path: &str,
	id: u64,
	cipher: Option<&Cipher>,
	counters: &Mutex<HashMap<String, u64>>,
	tx: &mpsc::Sender<Command>,
	keyframe: &AtomicBool,
) -> Result<()> {
	let broadcast = origin.request_broadcast(path).await?;
	let subscription = moq_net::track::Subscription::default()
		.with_priority(u8::MAX)
		.with_max_age(Duration::from_secs(2));
	let mut groups = broadcast.track(INPUT_TRACK)?.subscribe(subscription).await?.ordered();
	let mut expected = None;
	let aad = moqdesk_proto::e2e::aad(path, INPUT_TRACK);
	let mut last_counter = counters.lock().unwrap().get(path).copied();
	while let Some(mut group) = groups.next_group().await? {
		// A skipped or truncated group may have held a release; drop this viewer's held state rather than leave keys stuck.
		let lost = expected.is_some_and(|seq| group.sequence != seq);
		expected = Some(group.sequence + 1);
		if lost {
			tx.send(Command::Input {
				viewer: id,
				event: InputEvent::ReleaseAll,
			})?;
		}
		loop {
			let frame = match group.read_frame().await {
				Ok(Some(frame)) => frame,
				Ok(None) => break,
				Err(err) => {
					tracing::debug!(%err, "input group ended early");
					tx.send(Command::Input {
						viewer: id,
						event: InputEvent::ReleaseAll,
					})?;
					break;
				}
			};
			let payload = match cipher {
				None => frame.payload.to_vec(),
				Some(cipher) => match moqdesk_proto::e2e::open_input(cipher, &aad, &frame.payload) {
					Ok((counter, payload)) if last_counter.is_none_or(|last| counter > last) => {
						last_counter = Some(counter);
						counters.lock().unwrap().insert(path.to_owned(), counter);
						payload
					}
					Ok(_) => {
						tracing::warn!(viewer = %path, "replayed input frame");
						continue;
					}
					Err(_) => {
						tracing::warn!(viewer = %path, "input failed authentication (wrong key?)");
						continue;
					}
				},
			};
			match InputEvent::decode(&payload) {
				Ok(InputEvent::RequestKeyframe) => keyframe.store(true, Ordering::Relaxed),
				Ok(event) => tx.send(Command::Input { viewer: id, event })?,
				Err(err) => tracing::warn!(%err, "bad input message"),
			}
		}
	}
	Ok(())
}

/// The ticket is a bearer credential, so keep it readable by the owner only.
fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
	use std::io::Write;
	use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
	let mut file = std::fs::OpenOptions::new()
		.write(true)
		.create(true)
		.truncate(true)
		.mode(0o600)
		.open(path)?;
	file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
	file.write_all(contents.as_bytes())
}

fn random_token() -> String {
	let mut bytes = [0u8; 16];
	getrandom::fill(&mut bytes).expect("no OS randomness");
	bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
	a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The source IP the OS would use for outbound traffic. No packets are sent.
fn primary_ip() -> String {
	UdpSocket::bind("0.0.0.0:0")
		.and_then(|s| s.connect("192.0.2.1:9").and(s.local_addr()))
		.map(|a| a.ip().to_string())
		.unwrap_or_else(|_| "localhost".into())
}
