# moqdesk

Remote desktop (a VNC replacement) over [Media over QUIC](https://moq.dev), written in Rust.

```
moqdesk-host  ── X11 capture (MIT-SHM) → H.264 (OpenH264) ──▶ MoQ broadcast "desk"
     ▲                                                         ├─ catalog.json
     │ XTest                                                    ├─ video   (1 group = 1 GOP)
     │                                                          └─ cursor  (shape + position)
     └──────────── MoQ broadcast "viewer/<desk>/<id>" ◀── input ── moqdesk-view (winit + softbuffer)
```

## Why it beats VNC

| | VNC / RFB | moqdesk |
|---|---|---|
| Transport | One TCP stream: a lost packet stalls everything behind it | QUIC: each GOP is its own stream, input and cursor are separate tracks |
| Falling behind | Client drains every queued update | Viewer subscribes at the live edge and jumps to the newest GOP |
| Encoding | Rectangles (Tight/ZRLE) | H.264 in screen-content mode, only encoded when the screen changes |
| Cursor | Often drawn into the framebuffer | Own high-priority track; the viewer sets it as the local OS cursor, so pointer motion has zero network latency |
| Security | Optional, often plain text | TLS 1.3 always; direct mode pins a self-signed cert by fingerprint and checks an access token |
| Fan-out | One connection per viewer at the host | Publish once through [`moq-relay`](https://crates.io/crates/moq-relay); the relay fans out |
| Interop | RFB only | Standard [hang](https://docs.rs/hang) catalog and container, so other MoQ players can watch the stream |

## Quick start

System packages (Debian/Ubuntu): `nasm clang cmake pkg-config libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev`.

```sh
cargo build --release

# On the machine to share (X11):
./target/release/moqdesk-host
#   Share this ticket with viewers:
#     moqdesk-view moqdesk://192.168.1.20:4443/3f9c…#a1b2…

# On the viewing machine:
./target/release/moqdesk-view 'moqdesk://192.168.1.20:4443/3f9c…#a1b2…'
```

The ticket carries the address, a random access token (or `--token` / `MOQDESK_TOKEN`), and the SHA-256 fingerprint of the host's
self-signed certificate. Override the advertised address with `--public-host`. Use `--view-only` on either side to disable input.

### Through a relay

```sh
moq-relay --listen '[::]:4443' --listen-tls-cert cert.pem --listen-tls-key key.pem --auth-public 'anon/**'
moqdesk-host --relay https://relay.example.com:4443/anon --name office
moqdesk-view --url https://relay.example.com:4443/anon --name office
```

With `--auth-public 'anon/**'` anyone who can reach the relay can subscribe and publish, which is why relay mode encrypts
end to end by default (see below); use relay auth (JWTs) as well for anything real. For a self-signed test relay, pass `--relay-insecure` / `--insecure`.

### In a browser

The [browser viewer](web/) (WebTransport + WebCodecs, Chrome/Edge 97+, Firefox 114+, Safari 26.4+) is a static page; the
hosted copy is at <https://kareemn.github.io/moqdesk/>. In relay mode the host prints a ready-to-open link:

```sh
moqdesk-host --relay https://cdn.moq.dev/anon --name office-7f3a
#   Open in a browser (the key after # never leaves the browser):
#     https://kareemn.github.io/moqdesk/#relay=https%3A%2F%2Fcdn.moq.dev%2Fanon&name=office-7f3a&key=…
```

Everything after `#` stays in the browser, so neither the relay nor the page host sees the key. The page can also connect
straight to a host: paste its `moqdesk://` ticket into the form (the ticket's certificate fingerprint is pinned with
WebTransport `serverCertificateHashes`).

### End-to-end encryption

When publishing through a relay with input enabled, the host encrypts every video, cursor and input frame with AES-256-GCM
under a random key (or `MOQDESK_KEY`), binding each frame to its broadcast and track. Input frames also carry a counter so a
captured frame can't be replayed. The relay only forwards ciphertext and can't watch or inject input. Native viewers take the
key with `--key`. `--e2e` forces encryption for direct or view-only hosts; `--view-only --no-e2e` publishes plaintext for
generic players such as `https://moq.watch/anon/<name>?relay=https://cdn.moq.dev`.

### Headless mode

The viewer can run without a window, which is handy for scripting and tests:

```sh
moqdesk-view "$TICKET" --click 100,100 --type $'echo hi\n' --wait 1 --snapshot desk.png
```

## Host options

| Flag | Default | |
|---|---|---|
| `--listen` | `[::]:4443` | UDP address of the embedded MoQ server |
| `--display` | `$DISPLAY` | X display to capture |
| `--fps` | 30 | Frame-rate cap; unchanged frames are not encoded |
| `--bitrate` | 6000000 | Target bitrate (bps) |
| `--gop` | 5 | Seconds between forced keyframes (new MoQ groups) |
| `--relay` | | Publish through a relay instead of serving directly |

## Protocol

See [`crates/moqdesk-proto`](crates/moqdesk-proto/src/lib.rs). Video frames use the hang legacy container (H.264 Annex B with
inline SPS/PPS). Cursor and input messages are [postcard](https://docs.rs/postcard)-encoded enums. Keys are sent as USB HID
usages so any viewer platform can map them.

## Development

```sh
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test
scripts/e2e.sh   # needs Xvfb, xterm and xdpyinfo
cd web && npm ci && npm run dev   # browser viewer at http://localhost:5173/#relay=…&name=…&key=…
```

## Roadmap

- Wayland (PipeWire portal) and macOS/Windows hosts
- Hardware encoding (VA-API/NVENC), damage-based encoding, clipboard and audio tracks
