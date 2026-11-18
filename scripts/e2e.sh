#!/usr/bin/env bash
# End-to-end test: share a headless Xvfb desktop, connect a headless viewer,
# type a shell command into an xterm on the host, and check that it ran.
set -euo pipefail

BIN=${BIN:-target/release}
DISPLAY_NUM=${DISPLAY_NUM:-:77}
PORT=${PORT:-4477}
WORK=$(mktemp -d)
OUT="$WORK/typed.txt"
PIDS=()
cleanup() {
	for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
	[[ "${KEEP:-}" == 1 ]] || rm -rf "$WORK"
}
trap cleanup EXIT

Xvfb "$DISPLAY_NUM" -screen 0 1280x720x24 -nolisten tcp &
PIDS+=($!)
for _ in $(seq 50); do xdpyinfo -display "$DISPLAY_NUM" >/dev/null 2>&1 && break; sleep 0.1; done

DISPLAY=$DISPLAY_NUM xterm -geometry 80x20+0+0 -e bash --noprofile --norc &
PIDS+=($!)

"$BIN/moqdesk-host" --display "$DISPLAY_NUM" --listen "127.0.0.1:$PORT" --public-host localhost \
	--ticket-file "$WORK/ticket" >"$WORK/host.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 100); do [[ -s "$WORK/ticket" ]] && break; sleep 0.1; done
[[ -s "$WORK/ticket" ]] || { cat "$WORK/host.log"; echo "host did not start"; exit 1; }
sleep 1 # let xterm map

echo "== bad token is rejected"
BAD=$(sed -E 's|/[0-9a-f]+#|/wrong#|' "$WORK/ticket")
if timeout 15 "$BIN/moqdesk-view" "$BAD" --view-only --snapshot "$WORK/bad.png" --timeout 3 >/dev/null 2>&1; then
	echo "FAIL: viewer with a bad token got video"
	exit 1
fi

echo "== video + keyboard/mouse round trip"
timeout 60 "$BIN/moqdesk-view" "$(cat "$WORK/ticket")" --click 100,100 \
	--type "echo moqdesk-ok > $OUT"$'\n' --wait 1.5 --snapshot "$WORK/snap.png"

for _ in $(seq 30); do [[ -s "$OUT" ]] && break; sleep 0.1; done
if [[ "$(cat "$OUT" 2>/dev/null)" != "moqdesk-ok" ]]; then
	echo "FAIL: typed command did not run on the host"
	cat "$WORK/host.log"
	exit 1
fi
[[ -s "$WORK/snap.png" ]] || { echo "FAIL: no snapshot"; exit 1; }
echo "PASS"
