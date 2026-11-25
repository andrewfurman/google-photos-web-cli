# Protocol reference and attribution

The Google Photos RPC framing, payloads and field mappings were adapted into Rust
from [xob0t/google_photos_web_client](https://github.com/xob0t/google_photos_web_client),
commit `651aa268d50529cb6128c8a42c9cd4ce668f2189` (gpwc).

The following upstream license applies to the adapted material:

```text
MIT License

Copyright (c) 2024 xob0t

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

Runtime dependencies retain their respective licenses.

## Upload and album-creation request shapes

The upload flow (`swbisb` hash check, resumable `/_/upload/uploadmedia/interactive`
upload, `mdpdU` commit) follows the protocol description in
[benjamin-di/google-photos-mcp](https://github.com/benjamin-di/google-photos-mcp)
`docs/PAYLOADS.md` (MIT). The `OXvT9d` (create album) and `E1Cajb` (add to an owned
album) payloads follow [xob0t/Google-Photos-Toolkit](https://github.com/xob0t/Google-Photos-Toolkit)
`src/api/api.ts` (MIT). The Rust code is new; both were checked against the live web app
in November 2026.

## Date edits and album pages

The `DaSgWe` (edit date & time) and `snAcKc` (album page) request shapes follow
[xob0t/Google-Photos-Toolkit](https://github.com/xob0t/Google-Photos-Toolkit)
`src/api/api.ts` `setItemsTimestamp` / `getAlbumPage` (MIT); gpwc's `SetItemTimestamp`
documents the same RPC with milliseconds, but a live check in November 2026 showed seconds.
The Rust code is new.
