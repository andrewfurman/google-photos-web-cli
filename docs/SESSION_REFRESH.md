# Keeping a session alive

`google-photos-web-cli` signs in with a bundle of Google **browser cookies** saved in
`~/.config/google-photos-web-cli/.env` (Linux; see the README for other platforms). It is not
OAuth: there is no refresh token. This page explains why such a session dies, the two ways to
get a fresh one, and how to keep a session alive for months with `auth keepalive`.

Never commit, paste, or print cookie values or the `GPHOTOS_SESSION` value. Everything below
prints cookie *names* at most.

## 1. Why sessions expire

The bundle contains long-lived sign-in cookies (`SID`, `__Secure-1PSID`, `HSID`, ...; dated
a year or more out) and short-lived, rotating ones, chiefly **`__Secure-1PSIDTS`** (plus
`SIDCC` and friends). A signed-in browser periodically asks Google for a fresh
`__Secure-1PSIDTS`; the old value keeps working for a while and is then dropped.

- **A copied session fights its source.** If the same cookies are used by two holders (for
  example, a CLI copy of a Chrome profile that is still open and signed in), both rotate
  `__Secure-1PSIDTS`. Google then treats one holder's chain as superseded and invalidates
  it. In practice a copy imported from an always-signed-in Chrome died ~15-20 minutes after
  import. notebooklm-py documents the same thing: "concurrent users of copies of one
  `storage_state.json` can sign each other out", and in their tests a superseded snapshot was
  replaced "within roughly ten minutes" and failed "within about half an hour"
  ([notebooklm-py: auth cookie lifecycle][nlm]).
- **An idle session whose `__Secure-1PSIDTS` is never rotated** eventually stops working
  (observations vary from hours to a day or two).
- **Google revokes sessions** on sign-out ("Sign out of all devices", or signing out in the
  browser that created it), password change, account security checks / risk signals, and
  account policy. No refresh can recover from that: sign in again.

## 2. Option A (stopgap): import from another machine's signed-in Chrome

Any Chrome that is signed in to photos.google.com and has a DevTools port can donate its
cookies. Andrew's setup uses the PhoneClaw EC2 box:

- systemd service `pc-chrome-google`, profile `/home/phoneclaw/.chrome-google`, DevTools on
  `127.0.0.1:9233` (EC2-local only);
- reached from this VM with an AWS SSM port forward:

  ```bash
  aws ssm start-session --target <instance-id> \
    --document-name AWS-StartPortForwardingSession \
    --parameters '{"portNumber":["9233"],"localPortNumber":["19233"]}'
  ```

- cookies read over CDP (Playwright `chromium.connect_over_cdp("http://127.0.0.1:19233")`,
  then `context.cookies()`; do not call `browser.close()`, which would close the remote
  Chrome), written as a Netscape `cookies.txt`, and imported with
  `google-photos-web-cli auth cookies FILE`.

The family-newspaper repo automates exactly this:
`newspaper scripts/automation/gphotos_keepalive.py refresh`.

**Caveat:** the donor Chrome keeps running and rotating the same cookies, so the copy dies in
~15-20 minutes (section 1). It is fine for a one-off job, not for a long-lived session. Do
**not** run `auth keepalive` on such a copy: rotating it can knock the donor Chrome's session
out too.

## 3. Option B (recommended): a dedicated sign-in on this VM through noVNC

Give the CLI a session of its own: sign in once in a separate Chrome profile on this VM,
import the cookies, then stop that Chrome forever so the CLI is the only holder.

The pieces (all bound to localhost except the noVNC web port):

1. `Xvfb :55` virtual display, `openbox` window manager.
2. `google-chrome --user-data-dir=$HOME/.config/google-photos-web-cli/chrome-profile
   --remote-debugging-address=127.0.0.1 --remote-debugging-port=9244 --no-first-run
   https://photos.google.com/` on that display.
3. `x11vnc -localhost -nopw -display :55 -rfbport 5955 -forever -shared` (VNC only on
   localhost, so no VNC password is exposed to the network).
4. `websockify --web /usr/share/novnc 6082 localhost:5955` serving noVNC. On exe.dev, a port
   in 3000-9999 is reachable through the HTTPS proxy at
   `https://<vm>.exe.xyz:6082/vnc.html`. That proxy is **private** by default: only users
   with access to the VM can open it ([exe.dev proxy docs][exe-proxy]).
5. Open that URL, sign in at photos.google.com (complete any 2-step prompts).
6. Read the `google.com` cookies from Chrome over CDP (`Storage.getCookies` on
   `ws://127.0.0.1:9244/devtools/browser/...`), convert to Netscape format, and import:
   `google-photos-web-cli auth cookies /dev/stdin`.
7. **Stop that Chrome** and the desktop (Xvfb, openbox, x11vnc, websockify). Do not use
   Chrome's "Sign out": that revokes the session server-side. Just stop the process, and do not
   start it again with that profile while the CLI session is in use (or delete the profile
   directory once `auth keepalive` has succeeded).
8. Run `google-photos-web-cli auth keepalive` once by hand, then schedule it (section 4).

### Helper script

See [`scripts/novnc-signin.sh`](../scripts/novnc-signin.sh), which does steps 1-7:

```bash
scripts/novnc-signin.sh start          # prints https://<vm>.exe.xyz:6082/vnc.html; or: start 7000
#   ...sign in at photos.google.com in the browser desktop...
scripts/novnc-signin.sh check          # cookie NAMES only; confirms SID + __Secure-1PSIDTS
scripts/novnc-signin.sh import         # pipes cookies into 'auth cookies /dev/stdin', then stops everything
google-photos-web-cli auth keepalive
scripts/novnc-signin.sh status | stop
```

`import` uses only `python3` from the standard library (a minimal WebSocket client for the
DevTools protocol); no Playwright needed. The cookies travel only through the pipe into
`google-photos-web-cli auth cookies /dev/stdin` (the command takes a Netscape cookies.txt
*path*; `/dev/stdin` is that path for a pipe). No temporary file is written and no value is
printed. Options: `import --account N` for `/u/N/`, `--keep-running` to leave Chrome up (not
advised). PIDs and logs live in `~/.local/state/google-photos-web-cli/novnc/`. Set
`GPHOTOS_CLI=/path/to/google-photos-web-cli` to use a binary not on `PATH`, and
`GPHOTOS_SESSION_FILE` to import into a different session file. Required packages:
`xvfb openbox x11vnc websockify novnc` and Google Chrome.

## 4. Keeping it alive for months: `auth keepalive`

`google-photos-web-cli auth keepalive` does what a signed-in browser does in the background:

1. `POST https://accounts.google.com/RotateCookies` with the session's cookies, headers
   `Content-Type: application/json` and `Origin: https://accounts.google.com`, body
   `[000,"-0000000000000000000"]` (no redirects followed);
2. merges the `Set-Cookie` response (a fresh `__Secure-1PSIDTS`, `SIDCC`, ...) into the stored
   cookie jar and saves the session file atomically (temp file + rename, mode 0600);
3. runs the same Google Photos check as `auth status` (which also saves any cookies that
   rotate there).

It exits non-zero with a secret-free message if Google rejects the rotation (HTTP 401/403:
signed out or revoked), rate-limits it (429), or the Photos check fails. It refuses a session
supplied via the `GPHOTOS_SESSION` environment variable, because the rotated cookies could not
be written back.

This is the mechanism the long-running Gemini web clients use:

- **gemini-webapi** (HanaokaYuzu/Gemini-API) `rotate_1psidts()` POSTs
  `https://accounts.google.com/RotateCookies` with headers
  `{"Content-Type": "application/json", "Origin": "https://accounts.google.com"}` and data
  `'[000,"-0000000000000000000"]'`; on 401 it treats the cookies as invalid; it skips a
  rotation if the last one was under 60 seconds ago "to avoid 429 Too Many Requests"
  ([rotate_1psidts.py][g-rotate], [constants.py][g-consts]). Its client's `auto_refresh`
  defaults to `refresh_interval=600` seconds with ±15 s jitter and a 60 s floor
  ([client.py][g-client]). The README: "automatically refresh `__Secure-1PSIDTS` in the
  background as long as the process is alive", and recommends getting cookies "from a separate
  browser session and close it as soon as possible" ([README][g-readme]).
- **notebooklm-py** runs a 600-second keepalive by default for long-running servers and
  `notebooklm auth refresh` from an OS scheduler for idle profiles; its notes cite a
  `["identity.hfcr",600]` hint in the RotateCookies response as Google's own 10-minute
  cadence, and warn that cookie expiry dates are not a health check ([lifecycle notes][nlm]).
- An older gist shows the same RotateCookies flow ([szv99 gist][gist]).

**Schedule:** every ~10 minutes; never more than about once a minute.

```cron
# crontab -e   (user crontab on the VM; adjust the binary path)
*/10 * * * * timeout 120 $HOME/.cargo/bin/google-photos-web-cli auth keepalive --quiet >> $HOME/.local/state/google-photos-web-cli/keepalive.log 2>&1
# daily health check: mail/notify when it fails (replace the notifier with yours)
7 8 * * * timeout 120 $HOME/.cargo/bin/google-photos-web-cli auth status >/dev/null 2>&1 || echo "google-photos-web-cli session is dead; redo docs/SESSION_REFRESH.md Option B" | mail -s "Google Photos session expired" you@example.com
```

Or a systemd user timer:

```ini
# ~/.config/systemd/user/gphotos-keepalive.service
[Service]
Type=oneshot
ExecStart=%h/.cargo/bin/google-photos-web-cli auth keepalive --quiet
TimeoutStartSec=120

# ~/.config/systemd/user/gphotos-keepalive.timer
[Timer]
OnBootSec=2min
OnUnitActiveSec=10min
RandomizedDelaySec=30
[Install]
WantedBy=timers.target
```

`systemctl --user enable --now gphotos-keepalive.timer` (and `loginctl enable-linger $USER` so it
runs without a login).

Why this works: a session that **only the CLI uses** and whose `__Secure-1PSIDTS` is rotated
every ~10 minutes looks to Google like a browser that stays open, so it can last for months.
It is still revoked on password change, sign-out, or a security event; when `auth keepalive`
or `auth status` starts failing, redo Option B.

Other notes:

- Other CLI commands save rotated cookies too. If a long `search` overlaps a keepalive run,
  the last writer wins; the superseded `__Secure-1PSIDTS` stays valid for a while, and the next
  keepalive rotates again.
- RotateCookies is undocumented and Google may change or restrict it; automated access may be
  restricted under Google's terms. If it stops working, `auth keepalive` fails loudly and the
  session falls back to "sign in again".
- On platforms with device-bound session credentials (Chrome DBSC, currently Windows with a
  TPM), cookies may be bound to the signing-in device and refuse to work elsewhere. The Linux
  Chrome in Option B is not affected.

## 5. Secrets

- Never commit or print cookie values, the `.env` file, `cookies.txt` exports, or
  `GPHOTOS_SESSION`. The CLI and the helper script print cookie names only.
- The noVNC page gives control of a browser signed in to your Google account: keep the exe.dev
  proxy private, and stop the desktop as soon as the import is done.

[nlm]: https://github.com/teng-lin/notebooklm-py/blob/main/docs/auth-cookie-lifecycle.md
[g-rotate]: https://github.com/HanaokaYuzu/Gemini-API/blob/master/src/gemini_webapi/utils/rotate_1psidts.py
[g-consts]: https://github.com/HanaokaYuzu/Gemini-API/blob/master/src/gemini_webapi/constants.py
[g-client]: https://github.com/HanaokaYuzu/Gemini-API/blob/master/src/gemini_webapi/client.py
[g-readme]: https://github.com/HanaokaYuzu/Gemini-API#readme
[gist]: https://gist.github.com/szv99/f78c032736443fab51075bc45f9faf09
[exe-proxy]: https://exe.dev/docs/proxy.md
