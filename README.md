# moqdesk

Remote desktop (a VNC replacement) over [Media over QUIC](https://moq.dev), written in Rust.

```
moqdesk-host  ── X11 capture (MIT-SHM) → H.264 (OpenH264) ──▶ MoQ broadcast "desk"
     ▲                                                         ├─ catalog.json
     │ XTest                                                    ├─ video   (1 group = 1 GOP)
     │                                                          └─ cursor  (shape + position)
     └──────────── MoQ broadcast "viewer/<id>" ◀── input ── moqdesk-view (winit + softbuffer)
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

With `--auth-public 'anon/**'` anyone who can reach the relay can watch and send input, so use relay auth (JWTs) for
anything real, or start the host with `--view-only`. For a self-signed test relay, pass `--relay-insecure` / `--insecure`.

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
```

## Roadmap

- Browser viewer (WebTransport + WebCodecs)
- Wayland (PipeWire portal) and macOS/Windows hosts
- Hardware encoding (VA-API/NVENC), damage-based encoding, clipboard and audio tracks
