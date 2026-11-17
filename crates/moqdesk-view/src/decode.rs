//! H.264 decoding on a dedicated thread.

use std::sync::mpsc;

use anyhow::{Context, Result};
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;

/// An encoded access unit; `group` is the MoQ group sequence it arrived in.
pub struct Packet {
	pub group: u64,
	pub keyframe: bool,
	pub data: bytes::Bytes,
}

/// A decoded frame as 0RGB pixels, the layout softbuffer expects.
#[derive(Clone)]
pub struct Image {
	pub width: usize,
	pub height: usize,
	pub pixels: Vec<u32>,
}

pub fn run(rx: mpsc::Receiver<Packet>, mut on_frame: impl FnMut(Image)) -> Result<()> {
	let mut decoder = new_decoder()?;
	let mut rgb = Vec::new();
	let mut current_group = None;
	let mut backlog = Vec::new();

	while let Ok(first) = rx.recv() {
		backlog.push(first);
		backlog.extend(rx.try_iter());
		// If a newer GOP is already queued, everything before it is stale: skip straight to it.
		if let Some(start) = backlog.iter().rposition(|p| p.keyframe)
			&& start > 0
		{
			tracing::debug!(skipped = start, "dropping stale frames");
			backlog.drain(..start);
		}
		let mut latest = None;
		for packet in backlog.drain(..) {
			if current_group != Some(packet.group) {
				if !packet.keyframe {
					continue; // joined mid-GOP; wait for the next IDR
				}
				current_group = Some(packet.group);
			}
			match decoder.decode(&packet.data) {
				Ok(Some(yuv)) => {
					let (w, h) = yuv.dimensions();
					rgb.resize(w * h * 3, 0);
					yuv.write_rgb8(&mut rgb);
					latest = Some((w, h));
				}
				Ok(None) => {}
				Err(err) => {
					tracing::warn!(%err, "decode error; waiting for next keyframe");
					decoder = new_decoder()?;
					current_group = None;
				}
			}
		}
		if let Some((width, height)) = latest {
			let pixels = rgb
				.as_chunks::<3>()
				.0
				.iter()
				.map(|p| u32::from_be_bytes([0, p[0], p[1], p[2]]))
				.collect();
			on_frame(Image { width, height, pixels });
		}
	}
	Ok(())
}

fn new_decoder() -> Result<Decoder> {
	Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new()).context("failed to create decoder")
}
