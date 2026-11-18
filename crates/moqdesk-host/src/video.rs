//! Capture -> encode -> publish loop.
//!
//! Each MoQ group is one GOP that starts with an IDR frame. A new group starts when a
//! subscriber arrives, a viewer asks for one, the screen is resized, or `gop` elapses.
//! Viewers subscribe at the live edge, so a viewer that falls behind jumps to the newest
//! group instead of draining a backlog the way an RFB client would.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::capture::Capture;
use crate::clock::Clock;
use crate::encode::{H264Encoder, sps_profile};

// Stay well under moq-net's per-group limits (32 MiB / 8192 frames) by starting a new GOP early.
const MAX_GROUP_BYTES: usize = 16 << 20;
const MAX_GROUP_FRAMES: usize = 4096;

pub struct Settings {
	pub fps: f32,
	pub bitrate: u32,
	pub gop: Duration,
}

pub struct Tracks {
	pub catalog: moq_net::track::Producer,
	pub video: moq_net::track::Producer,
	/// Encrypt video frames end to end (the catalog stays readable).
	pub seal: Option<moqdesk_proto::e2e::Sealer>,
}

pub fn run(
	mut capture: Capture,
	mut tracks: Tracks,
	settings: Settings,
	keyframe: Arc<AtomicBool>,
	clock: Clock,
) -> Result<()> {
	let interval = Duration::from_secs_f32(1.0 / settings.fps);
	let mut encoder: Option<H264Encoder> = None;
	let mut group: Option<moq_net::group::Producer> = None;
	let mut group_start = Instant::now();
	let (mut group_bytes, mut group_frames) = (0usize, 0usize);
	let mut last_geometry = Instant::now();
	let mut prev: Vec<u8> = Vec::new();
	let mut catalog: Option<String> = None;
	let mut next = Instant::now();

	loop {
		let now = Instant::now();
		if next > now {
			std::thread::sleep(next - now);
		}
		next = (next + interval).max(Instant::now());

		if last_geometry.elapsed() > Duration::from_secs(1) {
			last_geometry = Instant::now();
			if capture.refresh_geometry()? {
				tracing::info!(size = ?capture.size(), "screen resized");
				encoder = None;
			}
		}

		// Catalog-first players subscribe to the catalog before video, so keep producing for them too.
		if !tracks.video.is_used() && !tracks.catalog.is_used() {
			if let Some(g) = group.take() {
				let _ = g.finish();
			}
			continue;
		}

		let force = group.is_none()
			|| encoder.is_none()
			|| keyframe.swap(false, Ordering::Relaxed)
			|| group_start.elapsed() >= settings.gop
			|| group_bytes >= MAX_GROUP_BYTES
			|| group_frames >= MAX_GROUP_FRAMES;
		let frame = match capture.grab() {
			Ok(frame) => frame,
			// The screen can shrink between geometry checks, making the grab rectangle invalid.
			Err(err) => {
				if capture.refresh_geometry()? {
					tracing::info!(size = ?capture.size(), "screen resized");
					encoder = None;
					continue;
				}
				return Err(err);
			}
		};
		// Unchanged frames are still encoded (as tiny P-frames): players with a jitter buffer stall and
		// show a spinner when frames stop arriving on a static desktop.
		prev.clear();
		prev.extend_from_slice(frame);

		let (w, h) = capture.size();
		let enc = match &mut encoder {
			Some(enc) => enc,
			None => encoder.insert(H264Encoder::new(w, h, settings.fps, settings.bitrate)?),
		};
		let Some(encoded) = enc.encode(&prev, force)? else {
			continue;
		};

		if encoded.keyframe {
			if let Some(g) = group.take() {
				let _ = g.finish();
			}
			publish_catalog(
				&mut tracks.catalog,
				&mut catalog,
				&encoded.data,
				(w, h),
				&settings,
				&clock,
			)?;
			group = Some(tracks.video.append_group()?);
			group_start = Instant::now();
			(group_bytes, group_frames) = (0, 0);
		}
		let Some(g) = group.as_mut() else { continue };
		group_bytes += encoded.data.len();
		group_frames += 1;
		let frame = hang::container::Frame {
			timestamp: clock.now(),
			payload: encoded.data.into(),
		};
		match &tracks.seal {
			Some(seal) => {
				let mut plain = bytes::BytesMut::new();
				frame.encode(&mut plain)?;
				g.write_frame(frame.timestamp, seal.seal(&plain))?;
			}
			None => frame.write_to(g)?,
		}
	}
}

fn publish_catalog(
	track: &mut moq_net::track::Producer,
	current: &mut Option<String>,
	keyframe: &[u8],
	(width, height): (u16, u16),
	settings: &Settings,
	clock: &Clock,
) -> Result<()> {
	let (profile, constraints, level) = sps_profile(keyframe).unwrap_or((0x42, 0xc0, 0x28));
	let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
		inline: true,
		profile,
		constraints,
		level,
	});
	config.coded_width = Some(width as u32);
	config.coded_height = Some(height as u32);
	config.framerate = Some(settings.fps as f64);
	config.bitrate = Some(settings.bitrate as u64);
	config.optimize_for_latency = Some(true);
	config.container = hang::catalog::Container::Legacy;

	let mut catalog = hang::Catalog::<()>::default();
	catalog
		.video
		.renditions
		.insert(moqdesk_proto::VIDEO_TRACK.to_string(), config);
	let json = catalog.to_json()?;
	if current.as_ref() == Some(&json) {
		return Ok(());
	}
	let mut group = track.append_group()?;
	group.write_frame(clock.now(), json.clone())?;
	group.finish()?;
	*current = Some(json);
	Ok(())
}
