//! Connection tickets for direct (relay-less) sessions.
//!
//! Format: `moqdesk://HOST:PORT/TOKEN#FINGERPRINT`, where `TOKEN` is the host's
//! access secret and `FINGERPRINT` is the hex SHA-256 of its self-signed certificate.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
	/// `host:port` (IPv6 hosts in brackets).
	pub addr: String,
	pub token: String,
	pub fingerprint: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TicketError {
	#[error("ticket must start with moqdesk://")]
	Scheme,
	#[error("ticket is missing the #fingerprint")]
	Fingerprint,
	#[error("ticket is missing the /token")]
	Token,
	#[error("ticket is missing host:port")]
	Addr,
}

impl Ticket {
	pub fn parse(s: &str) -> Result<Self, TicketError> {
		let rest = s.trim().strip_prefix("moqdesk://").ok_or(TicketError::Scheme)?;
		let (rest, fingerprint) = rest.split_once('#').ok_or(TicketError::Fingerprint)?;
		let (addr, token) = rest.split_once('/').ok_or(TicketError::Token)?;
		let token = token.trim_end_matches('/');
		if addr.is_empty() {
			return Err(TicketError::Addr);
		}
		if token.is_empty() || token.contains('/') {
			return Err(TicketError::Token);
		}
		if fingerprint.len() != 64 || !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
			return Err(TicketError::Fingerprint);
		}
		Ok(Self {
			addr: addr.to_string(),
			token: token.to_string(),
			fingerprint: fingerprint.to_ascii_lowercase(),
		})
	}

	/// The WebTransport URL a viewer dials.
	pub fn url(&self) -> String {
		format!("https://{}/{}", self.addr, self.token)
	}
}

impl fmt::Display for Ticket {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "moqdesk://{}/{}#{}", self.addr, self.token, self.fingerprint)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const FP: &str = "ab01ab01ab01ab01ab01ab01ab01ab01ab01ab01ab01ab01ab01ab01ab01ab01";

	#[test]
	fn roundtrip() {
		let t = Ticket {
			addr: "[::1]:4443".into(),
			token: "s3cret".into(),
			fingerprint: FP.into(),
		};
		assert_eq!(Ticket::parse(&t.to_string()).unwrap(), t);
		assert_eq!(t.url(), "https://[::1]:4443/s3cret");
	}

	#[test]
	fn errors() {
		assert_eq!(Ticket::parse("http://x"), Err(TicketError::Scheme));
		assert_eq!(Ticket::parse("moqdesk://h:1/t"), Err(TicketError::Fingerprint));
		assert_eq!(Ticket::parse(&format!("moqdesk://h:1#{FP}")), Err(TicketError::Token));
		assert_eq!(Ticket::parse("moqdesk://h:1/t#zz"), Err(TicketError::Fingerprint));
	}
}
