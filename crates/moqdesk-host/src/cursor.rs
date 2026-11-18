//! Publishes the X cursor shape and position on its own track, so the pointer
//! stays responsive even when video frames are queued behind a large update.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use moqdesk_proto::CursorUpdate;
use x11rb::protocol::xfixes::{ConnectionExt as _, GetCursorImageReply};

use crate::clock::Clock;

const POLL: Duration = Duration::from_millis(8);
/// Start a fresh group periodically so late joiners don't replay a long history.
const MAX_GROUP_FRAMES: usize = 600;

pub fn run(
	display: Option<&str>,
	track: moq_net::track::Producer,
	clock: Clock,
	seal: Option<moqdesk_proto::e2e::Sealer>,
) -> Result<()> {
	let (conn, _) = x11rb::connect(display).context("failed to connect to the X server")?;
	conn.xfixes_query_version(5, 0)?
		.reply()
		.context("XFixes is required for cursor capture")?;

	let mut group: Option<moq_net::group::Producer> = None;
	let mut serial = None;
	let mut pos = (i16::MIN, i16::MIN);
	let mut frames = 0;

	loop {
		let tick = Instant::now();
		if track.is_used() {
			let img = conn.xfixes_get_cursor_image()?.reply()?;
			let new_shape = serial != Some(img.cursor_serial);
			let update = if group.is_none() || new_shape || frames >= MAX_GROUP_FRAMES {
				if let Some(old) = group.take() {
					let _ = old.finish();
				}
				group = Some(track.append_group()?);
				frames = 0;
				serial = Some(img.cursor_serial);
				Some(shape(&img))
			} else if (img.x, img.y) != pos {
				Some(CursorUpdate::Position { x: img.x, y: img.y })
			} else {
				None
			};
			pos = (img.x, img.y);
			if let (Some(update), Some(g)) = (update, group.as_mut()) {
				let payload = update.encode();
				let payload = seal.as_ref().map_or(payload.clone(), |s| s.seal(&payload));
				g.write_frame(clock.now(), payload)?;
				frames += 1;
			}
		}
		std::thread::sleep(POLL.saturating_sub(tick.elapsed()));
	}
}

fn shape(img: &GetCursorImageReply) -> CursorUpdate {
	CursorUpdate::Shape {
		x: img.x,
		y: img.y,
		width: img.width,
		height: img.height,
		hot_x: img.xhot,
		hot_y: img.yhot,
		rgba: argb_premul_to_rgba(&img.cursor_image),
	}
}

/// XFixes cursors are premultiplied ARGB packed in u32s.
pub fn argb_premul_to_rgba(pixels: &[u32]) -> Vec<u8> {
	let mut out = Vec::with_capacity(pixels.len() * 4);
	for &p in pixels {
		let a = (p >> 24) as u8;
		let un = |c: u32| -> u8 {
			let c = c & 0xff;
			match a {
				0 => 0,
				255 => c as u8,
				_ => ((c * 255 + a as u32 / 2) / a as u32).min(255) as u8,
			}
		};
		out.extend_from_slice(&[un(p >> 16), un(p >> 8), un(p), a]);
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn unpremultiply() {
		assert_eq!(argb_premul_to_rgba(&[0xff112233]), vec![0x11, 0x22, 0x33, 0xff]);
		assert_eq!(argb_premul_to_rgba(&[0x00000000]), vec![0, 0, 0, 0]);
		assert_eq!(argb_premul_to_rgba(&[0x80404040]), vec![0x80, 0x80, 0x80, 0x80]);
	}
}
