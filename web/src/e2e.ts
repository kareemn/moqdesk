// Mirrors crates/moqdesk-proto/src/e2e.rs: payload = nonce(12) || AES-256-GCM(key, plaintext, aad).

const NONCE_LEN = 12;
const utf8 = new TextEncoder();

export class Cipher {
	private constructor(private key: CryptoKey) {}

	static async parse(encoded: string): Promise<Cipher> {
		const b64 = encoded.trim().replace(/-/g, "+").replace(/_/g, "/");
		const raw = Uint8Array.from(atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4)), (c) => c.charCodeAt(0));
		if (raw.length !== 32) throw new Error("key must be 32 bytes of base64url");
		const key = await crypto.subtle.importKey("raw", raw as BufferSource, "AES-GCM", false, ["encrypt", "decrypt"]);
		return new Cipher(key);
	}

	async seal(aad: string, plaintext: Uint8Array): Promise<Uint8Array> {
		const iv = crypto.getRandomValues(new Uint8Array(NONCE_LEN));
		const sealed = new Uint8Array(
			await crypto.subtle.encrypt({ name: "AES-GCM", iv, additionalData: utf8.encode(aad) }, this.key, plaintext as BufferSource),
		);
		const out = new Uint8Array(NONCE_LEN + sealed.length);
		out.set(iv);
		out.set(sealed, NONCE_LEN);
		return out;
	}

	async open(aad: string, sealed: Uint8Array): Promise<Uint8Array> {
		if (sealed.length < NONCE_LEN) throw new Error("frame failed authentication");
		const iv = sealed.subarray(0, NONCE_LEN);
		const body = sealed.subarray(NONCE_LEN);
		return new Uint8Array(
			await crypto.subtle.decrypt({ name: "AES-GCM", iv: iv as BufferSource, additionalData: utf8.encode(aad) }, this.key, body as BufferSource),
		);
	}

	/** Input frames are prefixed with a strictly increasing u64 (LE) counter for replay protection. */
	sealInput(aad: string, counter: bigint, event: Uint8Array): Promise<Uint8Array> {
		const plain = new Uint8Array(8 + event.length);
		new DataView(plain.buffer).setBigUint64(0, counter, true);
		plain.set(event, 8);
		return this.seal(aad, plain);
	}
}

export const aad = (broadcast: string, track: string) => `${broadcast}/${track}`;
