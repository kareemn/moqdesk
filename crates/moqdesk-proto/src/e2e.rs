//! End-to-end encryption for broadcasts that pass through an untrusted relay.
//!
//! Every frame payload becomes `nonce (12) || AES-256-GCM(key, plaintext, aad)`. The AAD binds a frame to
//! its broadcast path and track, so a relay can't splice frames between tracks or viewers. The key is
//! shared out of band (the `#fragment` of the browser link), so neither the relay nor the page host sees it.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("key must be {KEY_LEN} bytes of base64url")]
	BadKey,
	#[error("frame failed authentication")]
	Auth,
}

#[derive(Clone)]
pub struct Cipher {
	aead: Aes256Gcm,
	key: [u8; KEY_LEN],
}

impl Cipher {
	pub fn new(key: [u8; KEY_LEN]) -> Self {
		Self {
			aead: Aes256Gcm::new(&key.into()),
			key,
		}
	}

	pub fn generate() -> Self {
		let mut key = [0u8; KEY_LEN];
		getrandom::fill(&mut key).expect("no OS randomness");
		Self::new(key)
	}

	pub fn parse(encoded: &str) -> Result<Self, Error> {
		let bytes = URL_SAFE_NO_PAD.decode(encoded.trim()).map_err(|_| Error::BadKey)?;
		Ok(Self::new(bytes.try_into().map_err(|_| Error::BadKey)?))
	}

	/// base64url, no padding.
	pub fn encoded_key(&self) -> String {
		URL_SAFE_NO_PAD.encode(self.key)
	}

	pub fn seal(&self, aad: &str, plaintext: &[u8]) -> Vec<u8> {
		let mut nonce = [0u8; NONCE_LEN];
		getrandom::fill(&mut nonce).expect("no OS randomness");
		let sealed = self
			.aead
			.encrypt(
				Nonce::from_slice(&nonce),
				Payload {
					msg: plaintext,
					aad: aad.as_bytes(),
				},
			)
			.expect("AES-GCM encryption cannot fail for in-memory buffers");
		let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
		out.extend_from_slice(&nonce);
		out.extend_from_slice(&sealed);
		out
	}

	pub fn open(&self, aad: &str, sealed: &[u8]) -> Result<Vec<u8>, Error> {
		if sealed.len() < NONCE_LEN {
			return Err(Error::Auth);
		}
		let (nonce, body) = sealed.split_at(NONCE_LEN);
		self.aead
			.decrypt(
				Nonce::from_slice(nonce),
				Payload {
					msg: body,
					aad: aad.as_bytes(),
				},
			)
			.map_err(|_| Error::Auth)
	}
}

/// AAD for a track: `<broadcast path>/<track name>`.
pub fn aad(broadcast: &str, track: &str) -> String {
	format!("{broadcast}/{track}")
}

/// A cipher bound to one track's AAD.
#[derive(Clone)]
pub struct Sealer {
	pub cipher: Cipher,
	pub aad: String,
}

impl Sealer {
	pub fn new(cipher: &Cipher, broadcast: &str, track: &str) -> Self {
		Self {
			cipher: cipher.clone(),
			aad: aad(broadcast, track),
		}
	}

	pub fn seal(&self, plaintext: &[u8]) -> Vec<u8> {
		self.cipher.seal(&self.aad, plaintext)
	}

	pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, Error> {
		self.cipher.open(&self.aad, sealed)
	}
}

/// Input frames carry a strictly increasing counter so a captured frame can't be replayed.
pub fn seal_input(cipher: &Cipher, aad: &str, counter: u64, event: &[u8]) -> Vec<u8> {
	let mut plain = Vec::with_capacity(8 + event.len());
	plain.extend_from_slice(&counter.to_le_bytes());
	plain.extend_from_slice(event);
	cipher.seal(aad, &plain)
}

pub fn open_input(cipher: &Cipher, aad: &str, sealed: &[u8]) -> Result<(u64, Vec<u8>), Error> {
	let mut plain = cipher.open(aad, sealed)?;
	if plain.len() < 8 {
		return Err(Error::Auth);
	}
	let counter = u64::from_le_bytes(plain[..8].try_into().unwrap());
	plain.drain(..8);
	Ok((counter, plain))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roundtrip_and_tamper() {
		let c = Cipher::generate();
		let sealed = c.seal("desk/video", b"hello");
		assert_eq!(c.open("desk/video", &sealed).unwrap(), b"hello");
		assert!(c.open("desk/cursor", &sealed).is_err(), "AAD must bind the track");
		let mut bad = sealed.clone();
		*bad.last_mut().unwrap() ^= 1;
		assert!(c.open("desk/video", &bad).is_err());
		assert!(Cipher::generate().open("desk/video", &sealed).is_err());
	}

	#[test]
	fn key_encoding() {
		let c = Cipher::generate();
		let parsed = Cipher::parse(&c.encoded_key()).unwrap();
		assert_eq!(parsed.open("a", &c.seal("a", b"x")).unwrap(), b"x");
		assert!(Cipher::parse("short").is_err());
	}

	#[test]
	fn input_counter() {
		let c = Cipher::generate();
		let sealed = seal_input(&c, "viewer/desk/1/input", 7, b"ev");
		assert_eq!(
			open_input(&c, "viewer/desk/1/input", &sealed).unwrap(),
			(7, b"ev".to_vec())
		);
	}
}
