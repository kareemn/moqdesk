//! H.264 encoding tuned for screen content.

use anyhow::{Context, Result};
use openh264::OpenH264API;
use openh264::encoder::{
	BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer};

pub struct H264Encoder {
	encoder: Encoder,
	yuv: YUVBuffer,
	width: usize,
	height: usize,
}

pub struct EncodedFrame {
	/// Annex B access unit.
	pub data: Vec<u8>,
	pub keyframe: bool,
}

impl H264Encoder {
	pub fn new(width: u16, height: u16, fps: f32, bitrate: u32) -> Result<Self> {
		let config = EncoderConfig::new()
			.usage_type(UsageType::ScreenContentRealTime)
			.max_frame_rate(FrameRate::from_hz(fps))
			.bitrate(BitRate::from_bps(bitrate))
			.rate_control_mode(RateControlMode::Bitrate)
			.complexity(Complexity::Low)
			// Keyframes are requested explicitly so every MoQ group starts with one.
			.intra_frame_period(IntraFramePeriod::from_num_frames(0))
			.adaptive_quantization(false)
			.background_detection(false)
			.skip_frames(false);
		let encoder =
			Encoder::with_api_config(OpenH264API::from_source(), config).context("failed to create encoder")?;
		let (width, height) = (width as usize, height as usize);
		Ok(Self {
			encoder,
			yuv: YUVBuffer::new(width, height),
			width,
			height,
		})
	}

	/// Encode one BGRX frame. Returns `None` if the encoder skipped it.
	pub fn encode(&mut self, bgrx: &[u8], keyframe: bool) -> Result<Option<EncodedFrame>> {
		self.yuv.read_rgb(BgraSliceU8::new(bgrx, (self.width, self.height)));
		if keyframe {
			self.encoder.force_intra_frame();
		}
		let stream = self.encoder.encode(&self.yuv).context("encode failed")?;
		let frame_type = stream.frame_type();
		if matches!(frame_type, FrameType::Skip | FrameType::Invalid) {
			return Ok(None);
		}
		let data = stream.to_vec();
		Ok(Some(EncodedFrame {
			data,
			keyframe: matches!(frame_type, FrameType::IDR | FrameType::I),
		}))
	}
}

/// `(profile_idc, constraint_flags, level_idc)` from the first SPS in an Annex B stream.
pub fn sps_profile(annexb: &[u8]) -> Option<(u8, u8, u8)> {
	let mut i = 0;
	while i + 3 < annexb.len() {
		if annexb[i..i + 3] == [0, 0, 1] {
			let nal = &annexb[i + 3..];
			if nal.len() >= 4 && nal[0] & 0x1f == 7 {
				return Some((nal[1], nal[2], nal[3]));
			}
			i += 3;
		} else {
			i += 1;
		}
	}
	None
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn encodes_keyframe_with_sps() {
		let (w, h) = (64u16, 48u16);
		let mut enc = H264Encoder::new(w, h, 30.0, 1_000_000).unwrap();
		let frame: Vec<u8> = (0..w as usize * h as usize * 4).map(|i| (i % 251) as u8).collect();
		let first = enc.encode(&frame, true).unwrap().unwrap();
		assert!(first.keyframe);
		assert!(sps_profile(&first.data).is_some());
		let mut changed = frame.clone();
		changed[100] ^= 0xff;
		let second = enc.encode(&changed, false).unwrap().unwrap();
		assert!(!second.keyframe);
		let forced = enc.encode(&frame, true).unwrap().unwrap();
		assert!(forced.keyframe);
	}

	#[test]
	fn sps_parse() {
		assert_eq!(
			sps_profile(&[0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, 0xaa]),
			Some((0x42, 0xc0, 0x1f))
		);
		assert_eq!(sps_profile(&[0, 0, 1, 0x65, 1, 2, 3]), None);
	}
}
