//! Key identifiers. Viewers send USB HID usages (page 0x07) because every
//! platform can map its physical key codes to them; Linux hosts translate them
//! to evdev codes (X11 keycode = evdev + 8).

/// Map a USB HID keyboard usage to a Linux evdev key code.
pub fn hid_to_evdev(hid: u16) -> Option<u16> {
	const LETTERS: [u16; 26] = [
		30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
	];
	Some(match hid {
		0x04..=0x1d => LETTERS[(hid - 0x04) as usize],
		0x1e..=0x26 => hid - 0x1e + 2,  // 1-9
		0x27 => 11,                     // 0
		0x28 => 28,                     // Enter
		0x29 => 1,                      // Escape
		0x2a => 14,                     // Backspace
		0x2b => 15,                     // Tab
		0x2c => 57,                     // Space
		0x2d => 12,                     // -
		0x2e => 13,                     // =
		0x2f => 26,                     // [
		0x30 => 27,                     // ]
		0x31 | 0x32 => 43,              // \ and non-US #
		0x33 => 39,                     // ;
		0x34 => 40,                     // '
		0x35 => 41,                     // `
		0x36 => 51,                     // ,
		0x37 => 52,                     // .
		0x38 => 53,                     // /
		0x39 => 58,                     // Caps Lock
		0x3a..=0x43 => hid - 0x3a + 59, // F1-F10
		0x44 => 87,                     // F11
		0x45 => 88,                     // F12
		0x46 => 99,                     // Print Screen
		0x47 => 70,                     // Scroll Lock
		0x48 => 119,                    // Pause
		0x49 => 110,                    // Insert
		0x4a => 102,                    // Home
		0x4b => 104,                    // Page Up
		0x4c => 111,                    // Delete
		0x4d => 107,                    // End
		0x4e => 109,                    // Page Down
		0x4f => 106,                    // Right
		0x50 => 105,                    // Left
		0x51 => 108,                    // Down
		0x52 => 103,                    // Up
		0x53 => 69,                     // Num Lock
		0x54 => 98,                     // KP /
		0x55 => 55,                     // KP *
		0x56 => 74,                     // KP -
		0x57 => 78,                     // KP +
		0x58 => 96,                     // KP Enter
		0x59 => 79,                     // KP 1
		0x5a => 80,
		0x5b => 81,
		0x5c => 75,
		0x5d => 76,
		0x5e => 77,
		0x5f => 71,
		0x60 => 72,
		0x61 => 73,                      // KP 9
		0x62 => 82,                      // KP 0
		0x63 => 83,                      // KP .
		0x64 => 86,                      // non-US backslash
		0x65 => 127,                     // Menu
		0x67 => 117,                     // KP =
		0x68..=0x73 => hid - 0x68 + 183, // F13-F24
		0xe0 => 29,                      // Left Ctrl
		0xe1 => 42,                      // Left Shift
		0xe2 => 56,                      // Left Alt
		0xe3 => 125,                     // Left Meta
		0xe4 => 97,                      // Right Ctrl
		0xe5 => 54,                      // Right Shift
		0xe6 => 100,                     // Right Alt
		0xe7 => 126,                     // Right Meta
		_ => return None,
	})
}

pub const HID_LEFT_SHIFT: u16 = 0xe1;

/// Map an ASCII character to the HID usage that types it on a US layout, and whether Shift is needed.
pub fn ascii_to_hid(c: char) -> Option<(u16, bool)> {
	const SHIFTED_DIGITS: &str = "!@#$%^&*()";
	Some(match c {
		'a'..='z' => (0x04 + (c as u16 - 'a' as u16), false),
		'A'..='Z' => (0x04 + (c as u16 - 'A' as u16), true),
		'1'..='9' => (0x1e + (c as u16 - '1' as u16), false),
		'0' => (0x27, false),
		'\n' => (0x28, false),
		'\t' => (0x2b, false),
		' ' => (0x2c, false),
		'-' => (0x2d, false),
		'_' => (0x2d, true),
		'=' => (0x2e, false),
		'+' => (0x2e, true),
		'[' => (0x2f, false),
		'{' => (0x2f, true),
		']' => (0x30, false),
		'}' => (0x30, true),
		'\\' => (0x31, false),
		'|' => (0x31, true),
		';' => (0x33, false),
		':' => (0x33, true),
		'\'' => (0x34, false),
		'"' => (0x34, true),
		'`' => (0x35, false),
		'~' => (0x35, true),
		',' => (0x36, false),
		'<' => (0x36, true),
		'.' => (0x37, false),
		'>' => (0x37, true),
		'/' => (0x38, false),
		'?' => (0x38, true),
		_ => {
			let idx = SHIFTED_DIGITS.find(c)?;
			let hid = if idx == 9 { 0x27 } else { 0x1e + idx as u16 };
			(hid, true)
		}
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn letters_and_digits() {
		assert_eq!(hid_to_evdev(0x04), Some(30)); // A
		assert_eq!(hid_to_evdev(0x1d), Some(44)); // Z
		assert_eq!(hid_to_evdev(0x1e), Some(2)); // 1
		assert_eq!(hid_to_evdev(0x26), Some(10)); // 9
		assert_eq!(hid_to_evdev(0x27), Some(11)); // 0
		assert_eq!(hid_to_evdev(0x3a), Some(59)); // F1
		assert_eq!(hid_to_evdev(0x43), Some(68)); // F10
		assert_eq!(hid_to_evdev(0x68), Some(183)); // F13
		assert_eq!(hid_to_evdev(0x02), None);
	}

	#[test]
	fn ascii() {
		assert_eq!(ascii_to_hid('a'), Some((0x04, false)));
		assert_eq!(ascii_to_hid('Q'), Some((0x14, true)));
		assert_eq!(ascii_to_hid('!'), Some((0x1e, true)));
		assert_eq!(ascii_to_hid(')'), Some((0x27, true)));
		assert_eq!(ascii_to_hid('\n'), Some((0x28, false)));
		assert_eq!(ascii_to_hid('é'), None);
		for c in (0x20u8..0x7f).map(char::from) {
			let (hid, _) = ascii_to_hid(c).unwrap_or_else(|| panic!("no mapping for {c:?}"));
			assert!(hid_to_evdev(hid).is_some(), "{c:?}");
		}
	}
}
