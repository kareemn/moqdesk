import * as Moq from "@moq/net";
import { aad, Cipher } from "./e2e.ts";
import { codeToHid } from "./hid.ts";
import {
	Button,
	CURSOR_TRACK,
	decodeCursor,
	encodeInput,
	INPUT_TRACK,
	type InputEvent,
	splitHangFrame,
	VIDEO_TRACK,
	VIEWER_PREFIX,
} from "./proto.ts";

interface Target {
	url: URL;
	name: string;
	cipher?: Cipher;
	fingerprint?: Uint8Array;
	viewOnly: boolean;
}

const canvas = document.getElementById("screen") as HTMLCanvasElement;
const ctx = canvas.getContext("2d", { alpha: false })!;
const statusEl = document.getElementById("status")!;
const statsEl = document.getElementById("stats")!;
const form = document.getElementById("connect") as HTMLFormElement;
const stage = document.getElementById("stage")!;
const remoteCursor = document.createElement("img");
remoteCursor.id = "remote-cursor";
remoteCursor.hidden = true;
stage.appendChild(remoteCursor);

const setStatus = (s: string) => {
	statusEl.textContent = s;
};

function hexBytes(hex: string): Uint8Array {
	if (!/^([0-9a-fA-F]{2})+$/.test(hex)) throw new Error("bad fingerprint");
	return Uint8Array.from(hex.match(/../g)!.map((h) => parseInt(h, 16)));
}

async function parseTarget(fragment: string): Promise<Target | undefined> {
	const p = new URLSearchParams(fragment.replace(/^#/, ""));
	const viewOnly = p.get("view") === "1";
	const key = p.get("key");
	const cipher = key ? await Cipher.parse(key) : undefined;
	const name = p.get("name") || "desk";
	const relay = p.get("relay");
	// Optional certificate pin for relays with self-signed certificates (e.g. local testing).
	const fp = p.get("fp");
	const pin = fp ? hexBytes(fp) : undefined;
	if (relay) return { url: new URL(relay), name, cipher, fingerprint: pin, viewOnly };
	// Direct: the host's ticket moqdesk://HOST:PORT/TOKEN#FINGERPRINT.
	const ticket = p.get("ticket");
	if (ticket) {
		const m = /^moqdesk:\/\/([^/]+)\/([^#]+)#([0-9a-fA-F]{64})$/.exec(ticket.trim());
		if (!m) throw new Error("bad ticket");
		const fingerprint = hexBytes(m[3]);
		return { url: new URL(`https://${m[1]}/${m[2]}`), name, cipher, fingerprint, viewOnly };
	}
	return undefined;
}

function showForm() {
	form.hidden = false;
	setStatus("not connected");
	form.onsubmit = (e) => {
		e.preventDefault();
		const data = new FormData(form);
		const link = String(data.get("link") || "").trim();
		let fragment: string;
		// Check tickets first: they contain a `#` before the certificate fingerprint.
		if (link.startsWith("moqdesk://")) fragment = new URLSearchParams({ ticket: link }).toString();
		else if (link.includes("#")) fragment = link.slice(link.indexOf("#") + 1);
		else {
			const p = new URLSearchParams();
			for (const k of ["relay", "name", "key"]) {
				const v = String(data.get(k) || "").trim();
				if (v) p.set(k, v);
			}
			fragment = p.toString();
		}
		location.hash = fragment;
		location.reload();
	};
}

// Map client coordinates to remote pixels, accounting for object-fit: contain letterboxing.
function layout() {
	const r = canvas.getBoundingClientRect();
	const scale = Math.min(r.width / canvas.width, r.height / canvas.height);
	const w = canvas.width * scale;
	const h = canvas.height * scale;
	return { left: r.left + (r.width - w) / 2, top: r.top + (r.height - h) / 2, scale };
}

function toRemote(e: { clientX: number; clientY: number }) {
	const l = layout();
	const x = (e.clientX - l.left) / l.scale;
	const y = (e.clientY - l.top) / l.scale;
	return { x: Math.max(0, Math.min(canvas.width - 1, x)), y: Math.max(0, Math.min(canvas.height - 1, y)) };
}

function codecFromSps(annexb: Uint8Array): string | undefined {
	for (let i = 0; i + 4 < annexb.length; i++) {
		if (annexb[i] === 0 && annexb[i + 1] === 0 && annexb[i + 2] === 1 && (annexb[i + 3] & 0x1f) === 7) {
			const hex = (b: number) => b.toString(16).padStart(2, "0");
			return `avc1.${hex(annexb[i + 4])}${hex(annexb[i + 5])}${hex(annexb[i + 6])}`;
		}
	}
	return undefined;
}

let frames = 0;
setInterval(() => {
	statsEl.textContent = canvas.width > 1 ? `${canvas.width}×${canvas.height} · ${frames} fps` : "";
	frames = 0;
}, 1000);

function makeDecoder() {
	return new VideoDecoder({
		output: (frame) => {
			if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
				canvas.width = frame.displayWidth;
				canvas.height = frame.displayHeight;
			}
			ctx.drawImage(frame, 0, 0);
			frame.close();
			frames++;
		},
		error: (err) => console.warn("decoder error", err),
	});
}

/** Follow the live edge: whenever a newer group (GOP) arrives, abandon the current one. */
async function playVideo(desk: Moq.Broadcast.Consumer, target: Target) {
	const sub = desk.track(VIDEO_TRACK).subscribe({ priority: 1 });
	const trackAad = aad(target.name, VIDEO_TRACK);
	let decoder = makeDecoder();
	let codec: string | undefined;
	let next = sub.recvGroup();
	let current = -1;
	for (;;) {
		const group = await next;
		if (!group) return;
		next = sub.recvGroup();
		if (group.sequence <= current) {
			group.close();
			continue;
		}
		current = group.sequence;
		let first = true;
		for (;;) {
			const res = await Promise.race([
				group.readFrame().then(
					(f) => ({ frame: f }),
					() => ({ frame: undefined }),
				),
				next.then(() => ({ newer: true as const })),
			]);
			if ("newer" in res) break;
			const frame = res.frame;
			if (!frame) break;
			let payload = frame.payload;
			if (target.cipher) {
				try {
					payload = await target.cipher.open(trackAad, payload);
				} catch {
					setStatus("wrong key: can't decrypt video");
					continue;
				}
			}
			const { timestamp, payload: data } = splitHangFrame(payload);
			if (first) {
				const c = codecFromSps(data);
				if (c && (c !== codec || decoder.state !== "configured")) {
					if (decoder.state === "closed") decoder = makeDecoder();
					decoder.configure({ codec: c, optimizeForLatency: true, hardwareAcceleration: "no-preference" });
					codec = c;
				}
				// Drop any backlog so we never fall behind the live edge.
				if (decoder.decodeQueueSize > 2) {
					decoder.reset();
					if (codec) decoder.configure({ codec, optimizeForLatency: true });
				}
			}
			if (decoder.state !== "configured") continue;
			decoder.decode(new EncodedVideoChunk({ type: first ? "key" : "delta", timestamp, data }));
			first = false;
			setStatus(target.viewOnly ? "connected (view only)" : "connected");
		}
		group.close();
	}
}

async function playCursor(desk: Moq.Broadcast.Consumer, target: Target) {
	const sub = desk.track(CURSOR_TRACK).subscribe({ priority: 2 });
	const trackAad = aad(target.name, CURSOR_TRACK);
	let hot = { x: 0, y: 0 };
	let pos = { x: 0, y: 0 };
	const place = () => {
		const l = layout();
		remoteCursor.style.left = `${l.left - stage.getBoundingClientRect().left + (pos.x - hot.x) * l.scale}px`;
		remoteCursor.style.top = `${l.top - stage.getBoundingClientRect().top + (pos.y - hot.y) * l.scale}px`;
		remoteCursor.style.width = `${remoteCursor.naturalWidth * l.scale}px`;
	};
	window.addEventListener("resize", place);
	for (;;) {
		const group = await sub.recvGroup();
		if (!group) return;
		for (;;) {
			const frame = await group.readFrame().catch(() => undefined);
			if (!frame) break;
			let payload = frame.payload;
			if (target.cipher) {
				payload = await target.cipher.open(trackAad, payload).catch(() => new Uint8Array());
				if (!payload.length) continue;
			}
			const update = decodeCursor(payload);
			pos = { x: update.x, y: update.y };
			if (update.type === "Shape") {
				hot = { x: update.hotX, y: update.hotY };
				const img = new ImageData(new Uint8ClampedArray(update.rgba), update.width, update.height);
				const off = new OffscreenCanvas(update.width, update.height);
				off.getContext("2d")!.putImageData(img, 0, 0);
				const url = URL.createObjectURL(await off.convertToBlob());
				// Interactive: the local cursor takes the remote shape, so it never lags the hand.
				if (!target.viewOnly) canvas.style.cursor = `url(${url}) ${hot.x} ${hot.y}, default`;
				const old = remoteCursor.src;
				remoteCursor.src = url;
				await remoteCursor.decode().catch(() => {});
				if (old) URL.revokeObjectURL(old);
			}
			remoteCursor.hidden = !target.viewOnly;
			if (target.viewOnly) place();
		}
	}
}

function startInput(track: Moq.Track.Producer, target: Target, id: string) {
	const trackAad = aad(`${VIEWER_PREFIX}/${target.name}/${id}`, INPUT_TRACK);
	let group: Moq.Group.Producer | undefined;
	let started = 0;
	let counter = 0n;
	let chain = Promise.resolve();
	const send = (ev: InputEvent) => {
		// Encryption is async; chain sends so frames keep their order.
		chain = chain.then(async () => {
			const now = performance.now();
			// Rotate groups so the host's bounded subscription never buffers a long history.
			if (!group || now - started > 1000) {
				group?.close();
				group = track.appendGroup();
				started = now;
			}
			let payload = encodeInput(ev);
			if (target.cipher) payload = await target.cipher.sealInput(trackAad, ++counter, payload);
			group.writeFrame({ payload, timestamp: Moq.Time.Timestamp.now() });
		});
	};

	canvas.tabIndex = 0;
	canvas.focus();
	const buttons: Record<number, number> = { 0: Button.LEFT, 1: Button.MIDDLE, 2: Button.RIGHT, 3: Button.BACK, 4: Button.FORWARD };
	canvas.addEventListener("pointermove", (e) => {
		for (const ev of e.getCoalescedEvents?.().slice(-1) ?? [e]) send({ type: "PointerMove", ...toRemote(ev) });
	});
	canvas.addEventListener("pointerdown", (e) => {
		e.preventDefault();
		canvas.focus();
		canvas.setPointerCapture(e.pointerId);
		send({ type: "PointerMove", ...toRemote(e) });
		if (e.button in buttons) send({ type: "Button", button: buttons[e.button], pressed: true });
	});
	canvas.addEventListener("pointerup", (e) => {
		send({ type: "PointerMove", ...toRemote(e) });
		if (e.button in buttons) send({ type: "Button", button: buttons[e.button], pressed: false });
	});
	canvas.addEventListener("contextmenu", (e) => e.preventDefault());

	const scroll = { x: 0, y: 0 };
	canvas.addEventListener(
		"wheel",
		(e) => {
			e.preventDefault();
			const unit = e.deltaMode === WheelEvent.DOM_DELTA_LINE ? 1 / 3 : e.deltaMode === WheelEvent.DOM_DELTA_PAGE ? 3 : 1 / 100;
			scroll.x += e.deltaX * unit;
			scroll.y += e.deltaY * unit;
			const dx = Math.trunc(scroll.x);
			const dy = Math.trunc(scroll.y);
			if (dx || dy) {
				scroll.x -= dx;
				scroll.y -= dy;
				send({ type: "Scroll", dx, dy });
			}
		},
		{ passive: false },
	);

	// Keep remote modifiers in sync with the browser's view, e.g. when Shift was pressed before focus.
	const held = new Set<number>();
	const modifiers: [string, number, number][] = [
		["Control", 0xe0, 0xe4],
		["Shift", 0xe1, 0xe5],
		["Alt", 0xe2, 0xe6],
		["Meta", 0xe3, 0xe7],
	];
	const syncModifiers = (e: KeyboardEvent) => {
		for (const [name, left, right] of modifiers) {
			const down = e.getModifierState(name);
			if (down && !held.has(left) && !held.has(right)) {
				held.add(left);
				send({ type: "Key", hid: left, pressed: true });
			} else if (!down) {
				for (const hid of [left, right]) {
					if (held.delete(hid)) send({ type: "Key", hid, pressed: false });
				}
			}
		}
	};
	const key = (pressed: boolean) => (e: KeyboardEvent) => {
		const hid = codeToHid(e.code);
		if (hid === undefined) return;
		if (pressed) held.add(hid);
		else held.delete(hid);
		if (hid < 0xe0) syncModifiers(e);
		e.preventDefault();
		// The host's X server generates its own autorepeat.
		if (e.repeat) return;
		send({ type: "Key", hid, pressed });
	};
	canvas.addEventListener("keydown", key(true));
	canvas.addEventListener("keyup", key(false));
	const releaseAll = () => {
		held.clear();
		send({ type: "ReleaseAll" });
	};
	canvas.addEventListener("blur", releaseAll);
	window.addEventListener("blur", releaseAll);
	window.addEventListener("pagehide", releaseAll);
	send({ type: "RequestKeyframe" });
}

async function waitForBroadcast(requesting: Moq.Origin.Requesting): Promise<Moq.Broadcast.Consumer> {
	for (;;) {
		const active = requesting.active.peek();
		if (active) return active;
		await requesting.active.changed();
	}
}

async function run(target: Target) {
	setStatus(`connecting to ${target.url.host}…`);
	const subscribe = new Moq.Origin.Producer();
	const publish = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({
		url: target.url,
		consume: subscribe,
		publish: publish.consume(),
		webtransport: target.fingerprint
			? { serverCertificateHashes: [{ algorithm: "sha-256", value: target.fingerprint as BufferSource }] }
			: undefined,
		// The WebSocket fallback can't pin a certificate, so it must not carry a pinned connection's token.
		websocket: target.fingerprint ? { enabled: false } : undefined,
	});
	setStatus(`waiting for desktop "${target.name}"…`);

	if (!target.viewOnly) {
		const id = Array.from(crypto.getRandomValues(new Uint8Array(8)), (b) => b.toString(16).padStart(2, "0")).join("");
		const viewer = publish.createBroadcast(Moq.Path.from(VIEWER_PREFIX, target.name, id));
		const input = viewer.createTrack(INPUT_TRACK, { priority: 255 });
		viewer.announce();
		startInput(input, target, id);
	} else {
		canvas.classList.add("view-only");
	}

	const desk = await waitForBroadcast(subscribe.request(Moq.Path.from(target.name)));
	await Promise.race([playVideo(desk, target), playCursor(desk, target), connection.closed]);
	throw new Error("disconnected");
}

async function main() {
	let target: Target | undefined;
	try {
		target = await parseTarget(location.hash);
	} catch (err) {
		setStatus(String(err));
	}
	if (!target) return showForm();
	if (!("VideoDecoder" in window)) return setStatus("this browser lacks WebCodecs; use Chrome, Edge or Safari 17+");
	for (;;) {
		try {
			await run(target);
		} catch (err) {
			console.warn(err);
			setStatus(`${err instanceof Error ? err.message : err}; reconnecting…`);
		}
		await new Promise((r) => setTimeout(r, 2000));
		location.reload();
	}
}

void main();
