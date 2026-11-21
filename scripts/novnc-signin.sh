#!/usr/bin/env bash
# Dedicated Google sign-in for google-photos-web-cli through a private noVNC desktop.
# See docs/SESSION_REFRESH.md ("Option B"). Never prints cookie values.
set -euo pipefail

DISPLAY_NUM="${NOVNC_DISPLAY:-55}"
VNC_PORT="${NOVNC_VNC_PORT:-5955}"
CDP_PORT="${NOVNC_CDP_PORT:-9244}"
DEFAULT_WEB_PORT=6082
PROFILE="${NOVNC_CHROME_PROFILE:-$HOME/.config/google-photos-web-cli/chrome-profile}"
STATE="${NOVNC_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/google-photos-web-cli/novnc}"
START_URL="${NOVNC_START_URL:-https://photos.google.com/}"
CLI="${GPHOTOS_CLI:-google-photos-web-cli}"
NOVNC_WEB="${NOVNC_WEB:-/usr/share/novnc}"
SERVICES=(xvfb openbox chrome x11vnc websockify)

usage() {
  cat <<EOF
novnc-signin.sh: sign in to Google Photos in a private browser desktop, then import it.

Usage: scripts/novnc-signin.sh <start [PORT] | status | check | import [--account N] [--keep-running] | stop>

Commands:
  start [PORT]   Start Xvfb :$DISPLAY_NUM, openbox, Chrome (own profile, DevTools on
                 127.0.0.1:$CDP_PORT), x11vnc (localhost:$VNC_PORT) and websockify/noVNC on PORT
                 (3000-9999, default $DEFAULT_WEB_PORT). Prints the https://<vm>.exe.xyz:PORT URL.
  status         Show which processes are running and the noVNC URL.
  check          List Google cookie NAMES in that Chrome (no values); confirms sign-in.
  import         Read the google.com cookies over DevTools and pipe them (Netscape format)
                 into '$CLI auth cookies /dev/stdin'. Then stops the desktop and
                 Chrome so only the CLI uses the session (unless --keep-running).
    --account N      Google account index (/u/N/) passed to 'auth cookies' (default 0)
    --keep-running   Leave Chrome and the desktop running after the import (not advised)
  stop           Stop everything started by 'start'.

Examples:
  scripts/novnc-signin.sh start            # open the printed URL, sign in at photos.google.com
  scripts/novnc-signin.sh check
  scripts/novnc-signin.sh import           # then: google-photos-web-cli auth keepalive
  scripts/novnc-signin.sh stop

Environment overrides: GPHOTOS_CLI (CLI path), GPHOTOS_SESSION_FILE (session file),
NOVNC_CHROME_PROFILE, NOVNC_STATE_DIR, NOVNC_START_URL, NOVNC_DISPLAY, NOVNC_VNC_PORT,
NOVNC_CDP_PORT, NOVNC_WEB.
EOF
}

die() { echo "Error: $*" >&2; exit 1; }
pidfile() { echo "$STATE/$1.pid"; }
running() { local f; f="$(pidfile "$1")"; [[ -f "$f" ]] && kill -0 "$(cat "$f")" 2>/dev/null; }
web_port() { cat "$STATE/web.port" 2>/dev/null || echo "$DEFAULT_WEB_PORT"; }
vm_host() {
  local h
  h="$(hostname -f 2>/dev/null || hostname)"
  [[ "$h" == *.* ]] || h="$h.exe.xyz"
  echo "$h"
}
url() { echo "https://$(vm_host):$(web_port)/vnc.html?autoconnect=1&resize=remote"; }

launch() { # name, then command...
  local name="$1"; shift
  if running "$name"; then echo "$name already running (pid $(cat "$(pidfile "$name")"))"; return; fi
  nohup "$@" >"$STATE/$name.log" 2>&1 &
  echo $! >"$(pidfile "$name")"
}

wait_for() { # description, test command...
  local what="$1"; shift
  for _ in $(seq 1 50); do "$@" && return 0; sleep 0.2; done
  die "$what did not start; see logs in $STATE"
}

need() { for c in "$@"; do command -v "$c" >/dev/null || die "missing '$c' (apt install xvfb openbox x11vnc websockify novnc; Google Chrome)"; done; }

chrome_bin() {
  for c in google-chrome google-chrome-stable chromium chromium-browser; do
    command -v "$c" >/dev/null && { echo "$c"; return; }
  done
  die "Google Chrome not found"
}

cmd_start() {
  local port="${1:-$DEFAULT_WEB_PORT}"
  [[ "$port" =~ ^[0-9]+$ ]] && (( port >= 3000 && port <= 9999 )) \
    || die "PORT must be 3000-9999 so the exe.dev proxy can expose it"
  need Xvfb openbox x11vnc websockify
  [[ -f "$NOVNC_WEB/vnc.html" ]] || die "noVNC not found in $NOVNC_WEB (apt install novnc)"
  local chrome; chrome="$(chrome_bin)"
  mkdir -p "$STATE" "$PROFILE"
  chmod 700 "$STATE" "$PROFILE"
  echo "$port" >"$STATE/web.port"
  local display=":$DISPLAY_NUM"

  launch xvfb Xvfb "$display" -screen 0 1366x900x24 -nolisten tcp
  wait_for "Xvfb" test -S "/tmp/.X11-unix/X$DISPLAY_NUM"
  launch openbox env DISPLAY="$display" openbox
  launch chrome env DISPLAY="$display" "$chrome" \
    --user-data-dir="$PROFILE" \
    --remote-debugging-address=127.0.0.1 \
    --remote-debugging-port="$CDP_PORT" \
    --no-first-run --no-default-browser-check \
    --window-position=0,0 --window-size=1366,900 \
    "$START_URL"
  wait_for "Chrome DevTools" curl -fs -o /dev/null "http://127.0.0.1:$CDP_PORT/json/version"
  launch x11vnc x11vnc -localhost -nopw -display "$display" -rfbport "$VNC_PORT" -forever -shared
  wait_for "x11vnc" bash -c "exec 3<>/dev/tcp/127.0.0.1/$VNC_PORT" 2>/dev/null
  launch websockify websockify --web "$NOVNC_WEB" "$port" "localhost:$VNC_PORT"
  wait_for "websockify" curl -fs -o /dev/null "http://127.0.0.1:$port/vnc.html"

  cat <<EOF
Desktop running. Open (private to users with access to this VM):
  $(url)
Sign in at photos.google.com in that Chrome, then run:
  $0 check
  $0 import
EOF
}

cmd_stop() {
  local i name f
  for (( i=${#SERVICES[@]}-1; i>=0; i-- )); do
    name="${SERVICES[$i]}"; f="$(pidfile "$name")"
    [[ -f "$f" ]] || continue
    if kill -0 "$(cat "$f")" 2>/dev/null; then
      kill "$(cat "$f")" 2>/dev/null || true
      for _ in $(seq 1 50); do kill -0 "$(cat "$f")" 2>/dev/null || break; sleep 0.2; done
      kill -9 "$(cat "$f")" 2>/dev/null || true
      echo "stopped $name"
    fi
    rm -f "$f"
  done
}

cmd_status() {
  local name any=0
  for name in "${SERVICES[@]}"; do
    if running "$name"; then echo "$name: running (pid $(cat "$(pidfile "$name")"))"; any=1
    else echo "$name: stopped"; fi
  done
  (( any )) && echo "noVNC: $(url)"
  return 0
}

# Reads cookies over the Chrome DevTools Protocol with only the Python standard library
# (a minimal RFC 6455 client). mode=netscape writes a cookies.txt to stdout; mode=check prints
# cookie names only. Cookie values are never printed except into the 'auth cookies' pipe.
cdp_cookies() {
  python3 -I -c '
import base64, json, os, socket, struct, sys, urllib.parse, urllib.request
port, mode = int(sys.argv[1]), sys.argv[2]
DOMAINS = {".google.com", "google.com", ".photos.google.com", "photos.google.com"}
def fail(msg):
    sys.stderr.write("Error: " + msg + "\n"); sys.exit(1)
try:
    info = json.load(urllib.request.urlopen("http://127.0.0.1:%d/json/version" % port, timeout=5))
except Exception:
    fail("Chrome DevTools is not reachable on 127.0.0.1:%d; run start first" % port)
ws = urllib.parse.urlparse(info["webSocketDebuggerUrl"])
if ws.hostname not in ("127.0.0.1", "localhost"):
    fail("refusing a non-local DevTools endpoint")
sock = socket.create_connection((ws.hostname, ws.port), timeout=20)
key = base64.b64encode(os.urandom(16)).decode()
sock.sendall(("GET %s HTTP/1.1\r\nHost: %s:%d\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
              "Sec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n\r\n"
              % (ws.path, ws.hostname, ws.port, key)).encode())
buf = b""
while b"\r\n\r\n" not in buf:
    chunk = sock.recv(4096)
    if not chunk: fail("DevTools closed the connection")
    buf += chunk
head, buf = buf.split(b"\r\n\r\n", 1)
if b" 101 " not in head.split(b"\r\n", 1)[0] + b" ":
    fail("DevTools refused the WebSocket handshake")
def recvn(n):
    global buf
    while len(buf) < n:
        chunk = sock.recv(65536)
        if not chunk: fail("DevTools closed the connection")
        buf += chunk
    out, buf = buf[:n], buf[n:]
    return out
def send(opcode, data):
    n = len(data); hdr = bytes([0x80 | opcode])
    if n < 126: hdr += bytes([0x80 | n])
    elif n < 65536: hdr += bytes([0x80 | 126]) + struct.pack(">H", n)
    else: hdr += bytes([0x80 | 127]) + struct.pack(">Q", n)
    mask = os.urandom(4)
    sock.sendall(hdr + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))
def recv_json():
    parts = []
    while True:
        b0, b1 = recvn(2)
        n = b1 & 0x7F
        if n == 126: n = struct.unpack(">H", recvn(2))[0]
        elif n == 127: n = struct.unpack(">Q", recvn(8))[0]
        mask = recvn(4) if b1 & 0x80 else None
        data = recvn(n)
        if mask: data = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
        op = b0 & 0x0F
        if op == 8: fail("DevTools closed the connection")
        if op == 9: send(10, data); continue
        if op == 10: continue
        parts.append(data)
        if b0 & 0x80: return json.loads(b"".join(parts))
def call(i, method):
    send(1, json.dumps({"id": i, "method": method}).encode())
    while True:
        msg = recv_json()
        if msg.get("id") == i: return msg
cookies = None
for i, method in enumerate(("Storage.getCookies", "Network.getAllCookies"), 1):
    reply = call(i, method)
    if "result" in reply:
        cookies = reply["result"]["cookies"]; break
if cookies is None:
    fail("Chrome did not return cookies over DevTools")
google = [c for c in cookies if c.get("domain") in DOMAINS]
names = sorted({c["name"] for c in google})
signed_in = "SID" in names and "__Secure-1PSIDTS" in names
if mode == "check":
    print("%d google.com cookies: %s" % (len(google), ", ".join(names) or "none"))
    print("Signed in: " + ("yes (SID and __Secure-1PSIDTS present)" if signed_in
                           else "NO - sign in at photos.google.com first"))
    sys.exit(0 if signed_in else 1)
if not signed_in:
    fail("Chrome is not signed in to Google (no SID / __Secure-1PSIDTS); sign in first")
out = ["# Netscape HTTP Cookie File"]
for c in google:
    if any(ch in c["name"] + c["value"] + c["path"] for ch in "\t\r\n"):
        continue
    expires = 0 if c.get("session") or c.get("expires", -1) <= 0 else int(c["expires"])
    out.append("\t".join([
        ("#HttpOnly_" if c.get("httpOnly") else "") + c["domain"],
        "TRUE" if c["domain"].startswith(".") else "FALSE",
        c["path"], "TRUE" if c.get("secure") else "FALSE", str(expires), c["name"], c["value"]]))
sys.stderr.write("Read %d google.com cookies from Chrome (values not shown).\n" % len(google))
sys.stdout.write("\n".join(out) + "\n")
' "$CDP_PORT" "$1"
}

cmd_check() { cdp_cookies check; }

cmd_import() {
  local account=0 keep=0
  while (( $# )); do
    case "$1" in
      --account) [[ "${2:-}" =~ ^[0-9]+$ ]] || die "--account needs a number"; account="$2"; shift 2 ;;
      --keep-running) keep=1; shift ;;
      *) die "unknown import option '$1' (see --help)" ;;
    esac
  done
  command -v "$CLI" >/dev/null || die "'$CLI' not found; set GPHOTOS_CLI=/path/to/google-photos-web-cli"
  # Cookies flow only through the pipe: no temporary file, nothing printed.
  cdp_cookies netscape | "$CLI" auth cookies /dev/stdin --account "$account"
  if (( keep )); then
    echo "Chrome left running: it will rotate the same cookies and sign the CLI out within minutes. Run '$0 stop' soon." >&2
  else
    echo "Stopping Chrome and the desktop so only the CLI uses this session..."
    cmd_stop
    echo "Done. Next: $CLI auth keepalive   (then schedule it every 10 minutes)"
  fi
}

case "${1:-}" in
  start) shift; cmd_start "$@" ;;
  stop) cmd_stop ;;
  status) cmd_status ;;
  check) cmd_check ;;
  import) shift; cmd_import "$@" ;;
  -h|--help|help|"") usage ;;
  *) usage >&2; exit 2 ;;
esac
