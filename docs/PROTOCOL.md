# Protocol mappings

Google Photos uses undocumented `batchexecute` RPCs. These mappings are observed
behavior, not a Google-supported contract. No captured account data belongs here.
The baseline framing and metadata positions are credited in the third-party notices.

| Operation | RPC | Payload / returned fields |
| --- | --- | --- |
| Keyword search / bulk keyword paging | `EzkLib` | Query in payload `[7][0]`; result mode `[6]=2` selects the most-relevant set. `--sort recent` uses mode 1, the browser’s recent matching set. Paging limits are independent of mode. The legacy three-field query form is not used. |
| Named face groups | `yQelMe` | `[null,null,null,[[1,null,1]]]`; section type 1 under response `[1]` contains face groups. |
| People-only search | `EzkLib` | Selected face-group ID in filter `[7][1][0][0][0][0][1]`; mode 1 matches the People & Pets navigation's recent results. |
| Keyword plus person | `EzkLib` + `fDcn4b` | Fetch relevant keyword results and verify each result's per-photo face-group IDs locally. Google's mode 1 ignored the keyword in a combined filter, while mode 2 returned no matches for a known matching photo; neither is trusted for intersection. |
| All selected people | `EzkLib` + batched `fDcn4b` | Seed from one face group (or text/date source), then require every selected ID in each photo's face metadata. Never rely on an unverified multi-face wire expression. |
| Date-only timeline | `lcxiM` | `[page_token, before_timestamp_ms,100,null,1,3]`; date bounds are checked against returned capture dates, with timezone margins around server positioning/cutoff. |
| Extended photo info | `fDcn4b` | `[photo_id,1,null,null,1]`; the photo record is response `[0]`. |
| Photo / download info | `VrseUb` | `[photo_id,null,null,null,null]`; direct full-size/original URLs are response `[1]` and `[7]` when present. |
| Batched filenames | `EWgK9e` | Options require their own nested array; response `[0][1]` contains filename records. |
| List destination albums | `Z5xsfc` | `[page_token,null,null,null,1,null,3,50,[2],5]`; album rows in response `[0]`, next token in `[1]`; each row has ID at `[0]`. Title: since Nov 2026 in the trailing metadata object `{"72930366":[kind,title,…]}` (`[1][0]` is now the cover photo URL); older replies had it at `[1][0]`. |
| Add photos to an album (**write**) | `laUYf` | `[album_id,[2,null,[[[photo_id]],…],null,null,null,[1]]]`; confirmation: older replies echoed the requested IDs in `[4]`; since Nov 2026 `[4]` holds new album-item IDs and `[5][0]` pairs them, `[[requested_id, album_item_id, 2], …]`. Re-adding returns the same album-item IDs (no duplicates). The UI may then refresh its album list with `Z5xsfc`. |
| Add photos to an album you own (**write**) | `E1Cajb` | `[[photo_id,…],album_id]`. Google rejects `laUYf` with RPC code 3 for an album only you own (seen Nov 2026 on a newly created album), so `album-add` falls back to `E1Cajb`. Reply `[album_id,[album_item_id,…],[[[requested_id,album_item_id,n],…]]]`; `n` is 1 when added, 2 when the photo was already there. |
| Create an album (**write**) | `OXvT9d` | `[title,null,2]`; the new album ID is response `[0][0]`. Google allows duplicate titles, so the CLI looks the title up with `Z5xsfc` first and reuses it. |
| Already uploaded? | `swbisb` | `[[sha1_base64],null,3,0]` (standard padded base64). `[]` when no item has that hash, else `[[[hash,record,…],…]]` with `record = [media_key,[thumb,w,h,…],taken_ms,dedup_key,tz_offset_ms,uploaded_ms,…]`. |
| Upload bytes | HTTP, not batchexecute | `POST https://photos.google.com/_/upload/uploadmedia/interactive?authuser=N` with `x-goog-upload-protocol: resumable`, `x-goog-upload-command: start`, `x-goog-upload-header-content-length`, `x-goog-upload-file-name`, `x-goog-hash: sha1=…`, form content type, and a protobuf body `08 02 10 01 18 01 20 01 38 <size varint> 40 01`. The answer has `x-goog-upload-status: active` and `x-goog-upload-url`. The bytes go there in chunks that are multiples of `x-goog-upload-chunk-granularity`, with `x-goog-upload-offset` and command `upload`, then `upload, finalize` on the last chunk. The final body (`x-goog-upload-status: final`) is the upload token. |
| Create the uploaded item (**write**) | `mdpdU` | `[[[upload_token,file_name,modified_ms,2,2]]]`; answer `[[[upload_token,record,0]]]` in the `swbisb` record shape, `record[0]` = new media key. Without EXIF, `modified_ms` becomes the capture date. |
| Tag a face (**write**) | `k9FTge` | `[[null,media_key],[[[face_id],[group_id]]],[[null×20,[]]],[[],[null,null,[null,1]]]]`; `media_key` is extended-record field 11. Response `[0][0]` is the updated photo record; its field `[1][22]` must link `group_id` to `face_id`. |

Extended-photo record positions used by `inspect`: filename 2, capture timestamp 3,
timezone offset 4, bytes 5, dimensions 6–7, coordinates 9/13, people 18, albums 19,
camera 23, upload source 27, and storage attributes 30. Camera positions 0/1 are
manufacturer/model; 2 is lens when returned as text; 3–6 are focal length, f-number, ISO, and exposure seconds.
Coordinate integers are scaled by 1e7.

A per-photo face-group entry holds the group ID at `[0][0]` and the assigned name
at `[1][5]`. Unnamed groups are retained. Owner/contact records elsewhere in the
response are **not face tags**. Missing face-group metadata is reported as
`people_status=not_returned`, rather than inferring the photo contains no people.

Detected faces are extended-record field 21: `[[face_id],[left,top,right,bottom],…]`
with normalized coordinates. Field 20 links clusters to faces:
`[[cluster_id,[owner,n]],[face_id,…]|null,…]`. A face is tagged when one of its linked
cluster IDs is a face group in field 18; other linked clusters are internal and
ignored. `inspect` sorts faces left to right (then top to bottom) and numbers them from 0.
Field 18 can name a group whose linked face is absent from field 21; such tags are
reported under `people` only.

The web client sends `k9FTge` when "Edit faces" is confirmed with Done; picking a
person in the dialog sends nothing by itself. Removing a face and renaming groups
use other, unmapped requests.

Parsers support sparse trailing field objects. Tests must use synthetic values
and cover missing/unknown fields, malformed data, and identity boundaries. For
live verification, compare requested group IDs against each inspected result's
tags and compare a keyword result with the current web UI. Never publish live
RPC captures or signed media URLs.

Long searches fetch extended metadata in batches of at most 20 read RPCs. Each
request has a unique `pN` identifier, and response field 6 is matched to that ID;
arrival order is not trusted. Bounds and all selected group IDs are part of the
cursor scope. Captions are the extended record's description field, exposed as
both `caption` and the backward-compatible `description` key.
