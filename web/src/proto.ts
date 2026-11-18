// Postcard encoding of the types in crates/moqdesk-proto/src/lib.rs.

export const VIDEO_TRACK = "video";
export const CURSOR_TRACK = "cursor";
export const INPUT_TRACK = "input";
export const VIEWER_PREFIX = "viewer";

export const Button = { LEFT: 1, MIDDLE: 2, RIGHT: 3, BACK: 8, FORWARD: 9 } as const;

export type InputEvent =
	| { type: "PointerMove"; x: number; y: number }
	| { type: "Button"; button: number; pressed: boolean }
	| { type: "Scroll"; dx: number; dy: number }
	| { type: "Key"; hid: number; pressed: boolean }
	| { type: "ReleaseAll" }
	| { type: "RequestKeyframe" };

export type CursorUpdate =
	| { type: "Shape"; x: number; y: number; width: number; height: number; hotX: number; hotY: number; rgba: Uint8Array }
	| { type: "Position"; x: number; y: number };

class Writer {
	bytes: number[] = [];
	varint(v: number) {
		while (v >= 0x80) {
			this.bytes.push((v & 0x7f) | 0x80);
			v >>>= 7;
		}
		this.bytes.push(v);
	}
	i16(v: number) {
		this.varint(((v << 1) ^ (v >> 31)) >>> 0);
	}
	u8(v: number) {
		this.bytes.push(v & 0xff);
	}
}

const clampU16 = (v: number) => Math.max(0, Math.min(0xffff, Math.round(v)));
const clampI16 = (v: number) => Math.max(-0x8000, Math.min(0x7fff, Math.round(v)));

export function encodeInput(ev: InputEvent): Uint8Array {
	const w = new Writer();
	switch (ev.type) {
		case "PointerMove":
			w.varint(0);
			w.varint(clampU16(ev.x));
			w.varint(clampU16(ev.y));
			break;
		case "Button":
			w.varint(1);
			w.u8(ev.button);
			w.u8(ev.pressed ? 1 : 0);
			break;
		case "Scroll":
			w.varint(2);
			w.i16(clampI16(ev.dx));
			w.i16(clampI16(ev.dy));
			break;
		case "Key":
			w.varint(3);
			w.varint(clampU16(ev.hid));
			w.u8(ev.pressed ? 1 : 0);
			break;
		case "ReleaseAll":
			w.varint(4);
			break;
		case "RequestKeyframe":
			w.varint(5);
			break;
	}
	return new Uint8Array(w.bytes);
}

class Reader {
	pos = 0;
	constructor(private buf: Uint8Array) {}
	varint(): number {
		let v = 0;
		for (let shift = 0; shift < 35; shift += 7) {
			if (this.pos >= this.buf.length) throw new Error("truncated");
			const b = this.buf[this.pos++];
			v += (b & 0x7f) * 2 ** shift;
			if (!(b & 0x80)) return v;
		}
		throw new Error("varint overflow");
	}
	i16(): number {
		const z = this.varint();
		return z & 1 ? -((z + 1) / 2) : z / 2;
	}
	bytes(n: number): Uint8Array {
		if (this.pos + n > this.buf.length) throw new Error("truncated");
		const out = this.buf.subarray(this.pos, this.pos + n);
		this.pos += n;
		return out;
	}
}

export function decodeCursor(buf: Uint8Array): CursorUpdate {
	const r = new Reader(buf);
	const tag = r.varint();
	if (tag === 0) {
		const x = r.i16();
		const y = r.i16();
		const width = r.varint();
		const height = r.varint();
		const hotX = r.varint();
		const hotY = r.varint();
		const rgba = r.bytes(r.varint());
		return { type: "Shape", x, y, width, height, hotX, hotY, rgba };
	}
	if (tag === 1) return { type: "Position", x: r.i16(), y: r.i16() };
	throw new Error(`unknown cursor update ${tag}`);
}

/** hang container frame: QUIC varint timestamp (µs) followed by the codec payload. */
export function splitHangFrame(buf: Uint8Array): { timestamp: number; payload: Uint8Array } {
	const len = 1 << (buf[0] >> 6);
	let v = buf[0] & 0x3f;
	for (let i = 1; i < len; i++) v = v * 256 + buf[i];
	return { timestamp: v, payload: buf.subarray(len) };
}
