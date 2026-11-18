use std::time::Instant;

use moq_net::{Timescale, Timestamp};

/// Microsecond media clock shared by every track of a broadcast.
#[derive(Clone, Copy)]
pub struct Clock(Instant);

impl Clock {
	pub fn new() -> Self {
		Self(Instant::now())
	}

	pub fn now(&self) -> Timestamp {
		let micros = self.0.elapsed().as_micros() as u64;
		Timestamp::new(micros, Timescale::MICRO).expect("timestamp overflow")
	}
}
