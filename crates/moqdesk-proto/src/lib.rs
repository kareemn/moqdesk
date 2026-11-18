//! Wire format shared by `moqdesk-host` and `moqdesk-view`.
//!
//! A host publishes one MoQ broadcast (default path [`DEFAULT_DESK`]) with:
//! - `catalog.json`: a [hang](https://docs.rs/hang) catalog, so standard MoQ players can watch.
//! - [`VIDEO_TRACK`]: H.264 Annex B access units in the hang legacy container.
//!   Every group starts with an IDR frame, so a late or lagging viewer can join at any group.
//! - [`CURSOR_TRACK`]: [`CursorUpdate`]s. Each group starts with a [`CursorUpdate::Shape`].
//!
//! Each viewer publishes a broadcast under [`VIEWER_PREFIX`] with an [`INPUT_TRACK`]
//! carrying [`InputEvent`]s that the host replays.

mod keymap;
mod ticket;

pub use keymap::*;
pub use ticket::*;

use serde::{Deserialize, Serialize};

pub const DEFAULT_DESK: &str = "desk";
pub const VIDEO_TRACK: &str = "video";
pub const CURSOR_TRACK: &str = "cursor";
pub const INPUT_TRACK: &str = "input";
pub const VIEWER_PREFIX: &str = "viewer";

/// Pointer buttons, using X11 numbering.
pub mod button {
	pub const LEFT: u8 = 1;
	pub const MIDDLE: u8 = 2;
	pub const RIGHT: u8 = 3;
	pub const BACK: u8 = 8;
	pub const FORWARD: u8 = 9;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
	/// Absolute pointer position in remote framebuffer pixels.
	PointerMove {
		x: u16,
		y: u16,
	},
	Button {
		button: u8,
		pressed: bool,
	},
	/// Scroll in wheel notches; positive `dy` scrolls down, positive `dx` scrolls right.
	Scroll {
		dx: i16,
		dy: i16,
	},
	/// A physical key, identified by its USB HID usage (page 0x07).
	Key {
		hid: u16,
		pressed: bool,
	},
	/// Release every key and button this viewer holds (e.g. the viewer lost focus).
	ReleaseAll,
	/// Ask the host to start a new group with an IDR frame.
	RequestKeyframe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorUpdate {
	/// A new cursor image; `rgba` is straight (non-premultiplied) RGBA, `width * height * 4` bytes.
	Shape {
		x: i16,
		y: i16,
		width: u16,
		height: u16,
		hot_x: u16,
		hot_y: u16,
		rgba: Vec<u8>,
	},
	Position {
		x: i16,
		y: i16,
	},
}

#[derive(Debug, thiserror::Error)]
#[error("invalid message: {0}")]
pub struct DecodeError(#[from] postcard::Error);

impl InputEvent {
	pub fn encode(&self) -> Vec<u8> {
		postcard::to_stdvec(self).expect("serializing to a Vec cannot fail")
	}

	pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
		Ok(postcard::from_bytes(buf)?)
	}
}

impl CursorUpdate {
	pub fn encode(&self) -> Vec<u8> {
		postcard::to_stdvec(self).expect("serializing to a Vec cannot fail")
	}

	pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
		Ok(postcard::from_bytes(buf)?)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn input_roundtrip() {
		let events = [
			InputEvent::PointerMove { x: 1919, y: 1079 },
			InputEvent::Button {
				button: button::RIGHT,
				pressed: true,
			},
			InputEvent::Scroll { dx: -1, dy: 3 },
			InputEvent::Key {
				hid: 0x04,
				pressed: false,
			},
			InputEvent::ReleaseAll,
			InputEvent::RequestKeyframe,
		];
		for event in events {
			assert_eq!(InputEvent::decode(&event.encode()).unwrap(), event);
		}
	}

	#[test]
	fn cursor_roundtrip() {
		let shape = CursorUpdate::Shape {
			x: 5,
			y: -2,
			width: 2,
			height: 1,
			hot_x: 1,
			hot_y: 0,
			rgba: vec![1; 8],
		};
		assert_eq!(CursorUpdate::decode(&shape.encode()).unwrap(), shape);
		let pos = CursorUpdate::Position { x: 10, y: 20 };
		assert_eq!(CursorUpdate::decode(&pos.encode()).unwrap(), pos);
	}

	#[test]
	fn rejects_garbage() {
		assert!(InputEvent::decode(&[0xff, 0xff]).is_err());
	}
}
