//! Uploading local files the way photos.google.com's "Import photos from your computer" does:
//! 1. `swbisb` asks whether an item with the file's SHA-1 is already in the library (the web app
//!    then uploads nothing);
//! 2. a resumable upload to `/_/upload/uploadmedia/interactive` returns an upload token;
//! 3. `mdpdU` turns the token into a media item and returns its media key.
//!
//! Request shapes follow benjamin-di/google-photos-mcp docs/PAYLOADS.md and xob0t/gpwc (both MIT).
//! No captured account responses or credentials belong in this module or its tests.
use crate::protocol::field;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

pub const UPLOAD_PATH: &str = "/_/upload/uploadmedia/interactive";
/// Google's chunk granularity (X-Goog-Upload-Chunk-Granularity) when it does not send one.
pub const GRANULARITY: u64 = 256 * 1024;
/// Bytes per upload request: a multiple of the granularity.
pub const CHUNK: u64 = 64 * GRANULARITY;
const NAME_MAX: usize = 200;

/// A local photo or video, checked and hashed, ready to send.
#[derive(Debug, Clone)]
pub struct LocalFile {
    pub path: PathBuf,
    /// The filename the item gets in the library.
    pub name: String,
    pub size: u64,
    /// SHA-1 in standard padded base64, as Google writes it in `X-Goog-Hash` and `swbisb`.
    pub sha1: String,
    /// Last-modified time (ms since 1970); Google uses it when the file has no capture date.
    pub modified_ms: i64,
    pub media_type: &'static str,
}

/// A library item as `swbisb` and `mdpdU` describe it.
#[derive(Debug, Clone, Serialize)]
pub struct Stored {
    pub media_key: String,
    pub taken_at: Option<String>,
    pub uploaded_at: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    #[serde(skip)]
    pub uploaded_ms: Option<i64>,
}

/// The file's kind from its first bytes; only formats Google Photos keeps.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    let at = |range: std::ops::Range<usize>| head.get(range).unwrap_or(&[]);
    if head.starts_with(b"\xff\xd8\xff")
        || head.starts_with(b"\x89PNG\r\n\x1a\n")
        || head.starts_with(b"GIF87a")
        || head.starts_with(b"GIF89a")
        || (head.starts_with(b"RIFF") && at(8..12) == b"WEBP")
        || (head.starts_with(b"BM") && head.len() >= 26)
        || [&b"II*\x00"[..], b"MM\x00*", b"IIRO", b"IIRS", b"IIU\x00"].contains(&at(0..4))
    {
        return Some("photo");
    }
    if at(4..8) == b"ftyp" {
        let image_brands: [&[u8]; 11] = [
            b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1", b"avif",
            b"avis", b"crx ",
        ];
        return Some(if image_brands.contains(&at(8..12)) {
            "photo"
        } else {
            "video"
        });
    }
    if (head.starts_with(b"RIFF") && at(8..12) == b"AVI ")
        || [&b"moov"[..], b"mdat", b"wide", b"free", b"skip"].contains(&at(4..8))
        || head.starts_with(b"\x1a\x45\xdf\xa3")
        || head.starts_with(b"\x30\x26\xb2\x75\x8e\x66\xcf\x11")
        || head.starts_with(b"\x00\x00\x01\xba")
        || (head.len() > 376 && head[0] == 0x47 && head[188] == 0x47)
    {
        return Some("video");
    }
    None
}

/// The library filename: the base name without control characters, at most 200 characters.
pub fn library_name(path: &Path) -> Result<String> {
    let raw = path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{}: filename is not valid UTF-8", path.display()))?;
    let name: String = raw.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim_matches([' ', '.']);
    if name.is_empty() {
        bail!("{}: empty filename", path.display());
    }
    Ok(name.chars().take(NAME_MAX).collect())
}

/// Header values are Latin-1; other characters become '_' there (`mdpdU` carries the real name).
pub fn header_filename(name: &str) -> String {
    name.chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '_' })
        .collect()
}

/// Check that `path` is a non-empty photo or video, then hash it.
pub fn prepare(path: &Path) -> Result<LocalFile> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("{}: cannot read file", path.display()))?;
    if !meta.is_file() {
        bail!("{}: not a regular file", path.display());
    }
    if meta.len() == 0 {
        bail!("{}: file is empty", path.display());
    }
    let mut file =
        std::fs::File::open(path).with_context(|| format!("{}: cannot open", path.display()))?;
    let mut hash = Sha1::new();
    let mut head = Vec::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("{}: read failed", path.display()))?;
        if read == 0 {
            break;
        }
        if head.len() < 512 {
            head.extend_from_slice(&buffer[..read.min(512 - head.len())]);
        }
        hash.update(&buffer[..read]);
        size += read as u64;
    }
    let media_type = sniff(&head).with_context(|| {
        format!("{}: not a photo or video Google Photos accepts (JPEG, PNG, GIF, WebP, HEIC, AVIF, BMP, TIFF/raw, MP4, MOV, AVI, MKV/WebM, WMV, MPEG)", path.display())
    })?;
    let modified_ms = meta
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
        .map(|t| t.timestamp_millis())
        .unwrap_or_else(|| Utc::now().timestamp_millis());
    Ok(LocalFile {
        path: path.to_path_buf(),
        name: library_name(path)?,
        size,
        sha1: STANDARD.encode(hash.finalize()),
        modified_ms,
        media_type,
    })
}

fn varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Protobuf body of `X-Goog-Upload-Command: start`, as the web app sends it: fields 1-4 and 8
/// (meaning unknown) and field 7 = the size in bytes.
pub fn start_body(size: u64) -> Vec<u8> {
    let mut body = b"\x08\x02\x10\x01\x18\x01\x20\x01\x38".to_vec();
    varint(size, &mut body);
    body.extend_from_slice(b"\x40\x01");
    body
}

/// `swbisb`: library items whose SHA-1 matches.
pub fn hash_lookup_payload(sha1: &str) -> Value {
    json!([[sha1], null, 3, 0])
}

/// `mdpdU`: make a media item from an upload token. The trailing 2s are the web app's.
pub fn commit_payload(token: &str, name: &str, modified_ms: i64) -> Value {
    json!([[[token, name, modified_ms, 2, 2]]])
}

fn timestamp(ms: &Value, offset: &Value) -> Option<String> {
    let at = DateTime::<Utc>::from_timestamp_millis(ms.as_i64()?)?;
    let offset = offset
        .as_i64()
        .and_then(|o| i32::try_from(o / 1000).ok())
        .and_then(chrono::FixedOffset::east_opt);
    Some(match offset {
        Some(offset) => at.with_timezone(&offset).to_rfc3339(),
        None => at.to_rfc3339(),
    })
}

/// Entries of a `swbisb` or `mdpdU` answer: `[[[hash_or_token, record, …], …]]` where
/// `record = [media_key, [thumb, w, h, …], taken_ms, dedup_key, tz_offset_ms, uploaded_ms, …]`.
pub fn parse_stored(data: &Value) -> Vec<Stored> {
    field(data, 0)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let record = field(entry, 1);
            let key = field(record, 0).as_str()?;
            crate::client::check_id(key).ok()?;
            Some(Stored {
                media_key: key.to_owned(),
                taken_at: timestamp(field(record, 2), field(record, 4)),
                uploaded_at: timestamp(field(record, 5), &Value::Null),
                width: field(field(record, 1), 1).as_u64(),
                height: field(field(record, 1), 2).as_u64(),
                uploaded_ms: field(record, 5).as_i64(),
            })
        })
        .collect()
}

/// Upload tokens are opaque base64-ish text.
pub fn check_token(token: &str) -> Result<()> {
    if !(16..=8192).contains(&token.len())
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b))
    {
        bail!("Google Photos finished the upload without a valid upload token; the web upload protocol may have changed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = "AF1QipSynthetic001xxxxxxxxxxxxxxxxxxxxxxx";

    #[test]
    fn sniffs_photos_and_videos_and_rejects_other_files() {
        assert_eq!(sniff(b"\xff\xd8\xff\xe0rest"), Some("photo"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("photo"));
        assert_eq!(sniff(b"\0\0\0\x18ftypheic...."), Some("photo"));
        assert_eq!(sniff(b"\0\0\0\x18ftypqt  ...."), Some("video"));
        assert_eq!(sniff(b"\0\0\0\x18ftypisom...."), Some("video"));
        assert_eq!(sniff(b"%PDF-1.7"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn start_body_matches_the_recorded_web_request() {
        // 468 bytes = varint d4 03, as recorded from the web app.
        assert_eq!(
            start_body(468),
            b"\x08\x02\x10\x01\x18\x01\x20\x01\x38\xd4\x03\x40\x01"
        );
        assert_eq!(start_body(1)[9], 1);
    }

    #[test]
    fn payloads_match_the_web_app() {
        assert_eq!(
            hash_lookup_payload("AAAA").to_string(),
            r#"[["AAAA"],null,3,0]"#
        );
        assert_eq!(
            commit_payload("tok", "a.jpg", 1700000000000).to_string(),
            r#"[[["tok","a.jpg",1700000000000,2,2]]]"#
        );
    }

    #[test]
    fn parses_created_and_matching_items_and_skips_bad_keys() {
        let record = json!([
            KEY,
            ["https://example.test/t", 4000, 3000],
            1700000000000i64,
            "dedup",
            -18000000,
            1764460470000i64
        ]);
        let data = json!([[["token", record, 0], ["token", ["../bad"], 0]]]);
        let items = parse_stored(&data);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].media_key, KEY);
        assert_eq!(items[0].width, Some(4000));
        assert_eq!(
            items[0].taken_at.as_deref(),
            Some("2023-11-14T17:13:20-05:00")
        );
        assert!(items[0].uploaded_at.is_some());
        assert!(parse_stored(&json!([])).is_empty());
        assert!(parse_stored(&Value::Null).is_empty());
    }

    #[test]
    fn names_and_headers_are_safe() {
        assert_eq!(
            library_name(Path::new("/x/Café photo.jpg")).unwrap(),
            "Café photo.jpg"
        );
        assert_eq!(header_filename("Café photo.jpg"), "Caf_ photo.jpg");
        assert!(library_name(Path::new("/x/..")).is_err());
    }

    #[test]
    fn prepare_hashes_and_rejects_non_media() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("p.png");
        std::fs::write(&photo, b"\x89PNG\r\n\x1a\nabc").unwrap();
        let file = prepare(&photo).unwrap();
        assert_eq!(file.size, 11);
        assert_eq!(file.media_type, "photo");
        assert_eq!(file.sha1.len(), 28);
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"hello").unwrap();
        assert!(prepare(&text).is_err());
        let empty = dir.path().join("e.jpg");
        std::fs::write(&empty, b"").unwrap();
        assert!(prepare(&empty).is_err());
        assert!(check_token("short").is_err());
        assert!(check_token("AbCdEfGhIjKlMnOpQrSt-_=").is_ok());
    }
}
