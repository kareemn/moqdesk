//! Replays viewer input with XTest.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use anyhow::{Context, Result};
use moqdesk_proto::{InputEvent, hid_to_evdev};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
	BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

const KEY: (u8, u8) = (KEY_PRESS_EVENT, KEY_RELEASE_EVENT);
const BUTTON: (u8, u8) = (BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT);

pub enum Command {
	Input { viewer: u64, event: InputEvent },
	ViewerGone(u64),
}

#[derive(Default)]
struct Held {
	keys: HashSet<u8>,
	buttons: HashSet<u8>,
}

pub struct Injector {
	conn: RustConnection,
	root: Window,
	held: HashMap<u64, Held>,
	/// How many viewers hold each (press event, code), so one viewer can't release another's key or drag.
	counts: HashMap<(u8, u8), u32>,
}

impl Injector {
	pub fn new(display: Option<&str>) -> Result<Self> {
		let (conn, screen) = x11rb::connect(display).context("failed to connect to the X server")?;
		conn.xtest_get_version(2, 2)?
			.reply()
			.context("XTEST is required for input")?;
		let root = conn.setup().roots[screen].root;
		Ok(Self {
			conn,
			root,
			held: HashMap::new(),
			counts: HashMap::new(),
		})
	}

	pub fn run(mut self, rx: mpsc::Receiver<Command>) -> Result<()> {
		while let Ok(cmd) = rx.recv() {
			self.apply(cmd)?;
			while let Ok(cmd) = rx.try_recv() {
				self.apply(cmd)?;
			}
			self.conn.flush()?;
		}
		Ok(())
	}

	fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<()> {
		self.conn.xtest_fake_input(kind, detail, 0, self.root, x, y, 0)?;
		Ok(())
	}

	fn set(&mut self, (press, release): (u8, u8), code: u8, pressed: bool) -> Result<()> {
		let count = self.counts.entry((press, code)).or_default();
		let send = if pressed {
			*count += 1;
			*count == 1
		} else {
			*count = count.saturating_sub(1);
			*count == 0
		};
		if *count == 0 {
			self.counts.remove(&(press, code));
		}
		if send {
			self.fake(if pressed { press } else { release }, code, 0, 0)?;
		}
		Ok(())
	}

	fn apply(&mut self, cmd: Command) -> Result<()> {
		let (viewer, event) = match cmd {
			Command::Input { viewer, event } => (viewer, event),
			Command::ViewerGone(viewer) => (viewer, InputEvent::ReleaseAll),
		};
		match event {
			InputEvent::PointerMove { x, y } => self.fake(
				MOTION_NOTIFY_EVENT,
				0,
				x.min(i16::MAX as u16) as i16,
				y.min(i16::MAX as u16) as i16,
			)?,
			InputEvent::Button { button, pressed } => {
				if button == 0 {
					return Ok(());
				}
				let held = &mut self.held.entry(viewer).or_default().buttons;
				// Skip duplicates so a stuck state on one viewer can't double-release another's press.
				if pressed == held.contains(&button) {
					return Ok(());
				}
				if pressed {
					held.insert(button);
				} else {
					held.remove(&button);
				}
				self.set(BUTTON, button, pressed)?;
			}
			InputEvent::Scroll { dx, dy } => {
				for (delta, neg, pos) in [(dy, 4u8, 5u8), (dx, 6, 7)] {
					let button = if delta < 0 { neg } else { pos };
					for _ in 0..delta.unsigned_abs().min(32) {
						self.fake(BUTTON_PRESS_EVENT, button, 0, 0)?;
						self.fake(BUTTON_RELEASE_EVENT, button, 0, 0)?;
					}
				}
			}
			InputEvent::Key { hid, pressed } => {
				let Some(code) = hid_to_evdev(hid).and_then(|e| u8::try_from(e + 8).ok()) else {
					tracing::debug!(hid, "unmapped key");
					return Ok(());
				};
				let held = &mut self.held.entry(viewer).or_default().keys;
				if pressed && !held.insert(code) {
					// Auto-repeat from a viewer that already holds the key.
					self.fake(KEY_PRESS_EVENT, code, 0, 0)?;
				} else if pressed || held.remove(&code) {
					self.set(KEY, code, pressed)?;
				}
			}
			InputEvent::ReleaseAll => {
				if let Some(held) = self.held.remove(&viewer) {
					for code in held.keys {
						self.set(KEY, code, false)?;
					}
					for button in held.buttons {
						self.set(BUTTON, button, false)?;
					}
				}
			}
			InputEvent::RequestKeyframe => {}
		}
		Ok(())
	}
}
