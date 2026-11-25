//! Changing when photos were taken, like the web app's Info panel > Edit date & time.
//!
//! `DaSgWe` takes `[[[dedup_key, taken_seconds, utc_offset_seconds], …]]` and answers `[]`;
//! the request shape follows xob0t/Google-Photos-Toolkit `setItemsTimestamp` (MIT) and was
//! checked live: seconds, not milliseconds. The dedup key is extended-record (`fDcn4b`)
//! field 11. Album contents come from `snAcKc`. Everything here is pure and unit-tested;
//! the requests are sent by `client.rs`.
use crate::protocol::field;
use anyhow::{bail, Context, Result};
use chrono::{
    DateTime, Duration, FixedOffset, LocalResult, Months, NaiveDate, NaiveDateTime, NaiveTime,
    Offset, TimeZone, Utc,
};
use chrono_tz::Tz;
use serde_json::{json, Value};

/// Items per `DaSgWe` request.
pub const WRITE_BATCH: usize = 50;

/// What Google stores about an item's date.
#[derive(Debug, Clone, PartialEq)]
pub struct DatedItem {
    pub id: String,
    pub dedup_key: String,
    pub filename: Option<String>,
    pub taken_ms: i64,
    /// Milliseconds east of UTC; `None` when Google returned no offset.
    pub offset_ms: Option<i64>,
}

impl DatedItem {
    pub fn taken_at(&self) -> String {
        render(self.taken_ms, self.offset_ms.unwrap_or(0))
    }
}

/// A new capture time: whole seconds since 1970 and an offset in seconds east of UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub seconds: i64,
    pub offset_seconds: i32,
}

impl Target {
    pub fn taken_at(&self) -> String {
        render(self.seconds * 1000, i64::from(self.offset_seconds) * 1000)
    }
    /// Whether Google already stores exactly this time and offset for `item`.
    pub fn matches(&self, item: &DatedItem) -> bool {
        item.taken_ms == self.seconds * 1000
            && item.offset_ms == Some(i64::from(self.offset_seconds) * 1000)
    }
}

fn render(ms: i64, offset_ms: i64) -> String {
    let offset = i32::try_from(offset_ms / 1000)
        .ok()
        .and_then(FixedOffset::east_opt)
        .unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&offset).to_rfc3339())
        .unwrap_or_else(|| format!("{ms} ms"))
}

/// Which UTC offset the new time is in.
#[derive(Debug, Clone, Copy)]
pub enum Zone {
    /// Each item keeps the offset it has now.
    Keep,
    Fixed(FixedOffset),
    /// An IANA zone; the offset follows its daylight-saving rules at the new time.
    Named(Tz),
}

/// How the new times are chosen.
#[derive(Debug, Clone)]
pub enum Plan {
    /// Every item gets this wall-clock time.
    Set(NaiveDateTime),
    /// The earliest item gets this time; the others keep their distance from it.
    KeepSpacing(NaiveDateTime),
    /// Every item moves by this much on its own wall clock (offset unchanged).
    Shift(Shift),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shift {
    pub months: i32,
    pub seconds: i64,
}

/// `YYYY-MM-DD[THH:MM[:SS]][Z|±HH:MM]` (a space may replace `T`). A date alone means 00:00.
pub fn parse_date(text: &str) -> Result<(NaiveDateTime, Option<FixedOffset>)> {
    let bad = || {
        anyhow::anyhow!(
            "Invalid --date {text:?}: use YYYY-MM-DD, YYYY-MM-DDTHH:MM or YYYY-MM-DDTHH:MM:SS, optionally ending in Z or an offset like -05:00"
        )
    };
    let text = text.trim();
    let (body, offset) = if let Some(rest) = text.strip_suffix(['Z', 'z']) {
        (rest, Some(FixedOffset::east_opt(0).unwrap()))
    } else if text.len() > 10 {
        // An offset can only follow the time part (after position 10).
        match text.get(10..).and_then(|t| t.rfind(['+', '-'])) {
            Some(at) => {
                let at = at + 10;
                (
                    &text[..at],
                    Some(parse_offset(&text[at..]).map_err(|_| bad())?),
                )
            }
            None => (text, None),
        }
    } else {
        (text, None)
    };
    let body = body.trim().replacen(' ', "T", 1);
    let local = if body.len() == 10 {
        NaiveDate::parse_from_str(&body, "%Y-%m-%d")
            .map_err(|_| bad())?
            .and_time(NaiveTime::MIN)
    } else {
        NaiveDateTime::parse_from_str(&body, "%Y-%m-%dT%H:%M:%S")
            .or_else(|_| NaiveDateTime::parse_from_str(&body, "%Y-%m-%dT%H:%M"))
            .map_err(|_| bad())?
    };
    Ok((local, offset))
}

/// `±HH:MM`, `±HHMM`, `±HH`, `Z` or `UTC`, at most 14 hours from UTC.
pub fn parse_offset(text: &str) -> Result<FixedOffset> {
    let bad = || anyhow::anyhow!("Invalid offset {text:?}: use a form like -05:00, +0530 or Z");
    let text = text.trim();
    if matches!(text, "Z" | "z" | "UTC" | "utc") {
        return Ok(FixedOffset::east_opt(0).unwrap());
    }
    let sign = match text.chars().next() {
        Some('+') => 1,
        Some('-') => -1,
        _ => return Err(bad()),
    };
    let digits: String = text[1..].chars().filter(|c| *c != ':').collect();
    if !digits.bytes().all(|b| b.is_ascii_digit()) || ![2, 4].contains(&digits.len()) {
        return Err(bad());
    }
    let hours: i32 = digits[..2].parse().map_err(|_| bad())?;
    let minutes: i32 = match &digits[2..] {
        "" => 0,
        m => m.parse().map_err(|_| bad())?,
    };
    if minutes >= 60 || hours * 60 + minutes > 14 * 60 {
        return Err(bad());
    }
    Ok(FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60)).unwrap())
}

pub fn parse_zone(text: &str) -> Result<Tz> {
    text.trim().parse::<Tz>().map_err(|_| {
        anyhow::anyhow!("Unknown time zone {text:?}: use an IANA name like America/New_York")
    })
}

/// `+1y`, `-2h`, `+1d12h`, `-1y2mo`: an optional sign for the whole value, then number+unit
/// pairs. Units: y (years), mo (months), w, d, h, m or min (minutes), s.
pub fn parse_shift(text: &str) -> Result<Shift> {
    let bad = || {
        anyhow::anyhow!(
            "Invalid --shift {text:?}: use forms like +1y, -2h, +1d12h or -1y2mo (units y, mo, w, d, h, m, s)"
        )
    };
    let text = text.trim();
    let (sign, mut rest) = match text.as_bytes().first() {
        Some(b'-') => (-1i64, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
    };
    if rest.is_empty() {
        return Err(bad());
    }
    let mut shift = Shift::default();
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let units = rest[digits..]
            .bytes()
            .take_while(u8::is_ascii_alphabetic)
            .count();
        if digits == 0 || units == 0 || digits > 9 {
            return Err(bad());
        }
        let n: i64 = rest[..digits].parse().map_err(|_| bad())?;
        match &rest[digits..digits + units] {
            "y" => shift.months += i32::try_from(n * 12).map_err(|_| bad())?,
            "mo" => shift.months += i32::try_from(n).map_err(|_| bad())?,
            "w" => shift.seconds += n * 7 * 86_400,
            "d" => shift.seconds += n * 86_400,
            "h" => shift.seconds += n * 3600,
            "m" | "min" => shift.seconds += n * 60,
            "s" => shift.seconds += n,
            _ => return Err(bad()),
        }
        rest = &rest[digits + units..];
    }
    if shift == Shift::default() {
        bail!("--shift {text:?} would not change anything");
    }
    shift.months *= sign as i32;
    shift.seconds *= sign;
    Ok(shift)
}

fn own_offset(item: &DatedItem) -> FixedOffset {
    item.offset_ms
        .and_then(|ms| i32::try_from(ms / 1000).ok())
        .and_then(FixedOffset::east_opt)
        .unwrap_or_else(|| FixedOffset::east_opt(0).unwrap())
}

/// The offset of wall-clock time `local` for this item.
fn offset_for_local(zone: Zone, local: NaiveDateTime, item: &DatedItem) -> Result<FixedOffset> {
    Ok(match zone {
        Zone::Keep => own_offset(item),
        Zone::Fixed(offset) => offset,
        Zone::Named(tz) => match tz.from_local_datetime(&local) {
            LocalResult::Single(t) => t.offset().fix(),
            // Clocks went back: the earlier of the two, as the web app's picker shows first.
            LocalResult::Ambiguous(first, _) => first.offset().fix(),
            LocalResult::None => {
                bail!("{local} does not exist in {tz} (clocks skip that hour); choose another time")
            }
        },
    })
}

fn offset_at(zone: Zone, seconds: i64, item: &DatedItem) -> FixedOffset {
    match zone {
        Zone::Keep => own_offset(item),
        Zone::Fixed(offset) => offset,
        Zone::Named(tz) => tz
            .offset_from_utc_datetime(
                &DateTime::<Utc>::from_timestamp(seconds, 0)
                    .unwrap_or_default()
                    .naive_utc(),
            )
            .fix(),
    }
}

fn target(local: NaiveDateTime, offset: FixedOffset) -> Target {
    Target {
        seconds: (local - Duration::seconds(i64::from(offset.local_minus_utc())))
            .and_utc()
            .timestamp(),
        offset_seconds: offset.local_minus_utc(),
    }
}

/// The new time for each item, in order. Google stores whole seconds.
pub fn plan(items: &[DatedItem], how: &Plan, zone: Zone) -> Result<Vec<Target>> {
    match how {
        Plan::Set(local) => items
            .iter()
            .map(|item| Ok(target(*local, offset_for_local(zone, *local, item)?)))
            .collect(),
        Plan::KeepSpacing(local) => {
            let Some(earliest) = items.iter().min_by_key(|i| i.taken_ms) else {
                return Ok(Vec::new());
            };
            let first = target(*local, offset_for_local(zone, *local, earliest)?);
            let delta = first.seconds * 1000 - earliest.taken_ms;
            Ok(items
                .iter()
                .map(|item| {
                    let seconds = (item.taken_ms + delta).div_euclid(1000);
                    Target {
                        seconds,
                        offset_seconds: offset_at(zone, seconds, item).local_minus_utc(),
                    }
                })
                .collect())
        }
        Plan::Shift(shift) => items
            .iter()
            .map(|item| {
                let offset = own_offset(item);
                let local = DateTime::<Utc>::from_timestamp_millis(item.taken_ms)
                    .context("Google returned an impossible capture time")?
                    .with_timezone(&offset)
                    .naive_local();
                let months = Months::new(shift.months.unsigned_abs());
                let moved = if shift.months >= 0 {
                    local.checked_add_months(months)
                } else {
                    local.checked_sub_months(months)
                }
                .and_then(|t| t.checked_add_signed(Duration::seconds(shift.seconds)))
                .context("The shifted date is out of range")?;
                Ok(target(moved, offset))
            })
            .collect(),
    }
}

/// `DaSgWe` request for these changes.
pub fn write_payload(changes: &[(&DatedItem, Target)]) -> Value {
    json!([changes
        .iter()
        .map(|(item, t)| json!([item.dedup_key, t.seconds, t.offset_seconds]))
        .collect::<Vec<_>>()])
}

/// An item's date from its `fDcn4b` reply (`[record, …]`): ID 0, filename 2, capture time 3,
/// offset 4, dedup key 11.
pub fn parse_dated(ext: &Value, requested: &str) -> Result<DatedItem> {
    let record = field(ext, 0);
    let dedup_key = field(record, 11)
        .as_str()
        .filter(|k| {
            !k.is_empty()
                && k.len() <= 256
                && k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        })
        .context("Google returned no item key for this photo (not in your library?)")?;
    let taken_ms = field(record, 3)
        .as_i64()
        .context("Google returned no capture date for this photo")?;
    Ok(DatedItem {
        id: requested.to_owned(),
        dedup_key: dedup_key.to_owned(),
        filename: field(record, 2)
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        taken_ms,
        offset_ms: field(record, 4).as_i64(),
    })
}

/// `snAcKc` (album page): `[album_id, page_token, null, auth_key]`.
pub fn album_page_payload(album_id: &str, token: Option<&str>) -> Value {
    json!([album_id, token, null, null])
}

/// Item IDs from an `snAcKc` reply (`[_, items, next_token, album, …]`) and the next page
/// token (`""` or null on the last page).
pub fn parse_album_page(data: &Value) -> Result<(Vec<String>, Option<String>)> {
    if !data.is_array() {
        bail!("Google Photos returned an unexpected album page");
    }
    let ids = field(data, 1)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| field(item, 0).as_str())
        .filter(|id| crate::client::check_id(id).is_ok())
        .map(str::to_owned)
        .collect();
    let next = field(data, 2)
        .as_str()
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    Ok((ids, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, taken_ms: i64, offset_ms: Option<i64>) -> DatedItem {
        DatedItem {
            id: id.into(),
            dedup_key: format!("dedup-{id}"),
            filename: None,
            taken_ms,
            offset_ms,
        }
    }
    fn at(text: &str) -> NaiveDateTime {
        parse_date(text).unwrap().0
    }
    const EST: i64 = -18_000_000;

    #[test]
    fn parses_dates_with_and_without_time_and_offset() {
        assert_eq!(at("2026-11-22"), at("2026-11-22T00:00:00"));
        assert_eq!(at("2026-11-22 10:00"), at("2026-11-22T10:00:00"));
        let (local, offset) = parse_date("2025-11-29T18:54:30-05:00").unwrap();
        assert_eq!(local, at("2025-11-29T18:54:30"));
        assert_eq!(offset.unwrap().local_minus_utc(), -18000);
        assert_eq!(
            parse_date("2026-01-01T00:00Z").unwrap().1.unwrap(),
            FixedOffset::east_opt(0).unwrap()
        );
        assert_eq!(
            parse_date("2026-01-01T09:00+0530")
                .unwrap()
                .1
                .unwrap()
                .local_minus_utc(),
            19800
        );
        for bad in [
            "",
            "2026-13-01",
            "22/11/2026",
            "2026-11-22T25:00",
            "2026-11-22T10:00+99:00",
            "tomorrow",
        ] {
            assert!(parse_date(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_offsets_zones_and_shifts() {
        assert_eq!(parse_offset("-05:00").unwrap().local_minus_utc(), -18000);
        assert_eq!(parse_offset("+0530").unwrap().local_minus_utc(), 19800);
        assert_eq!(parse_offset("+01").unwrap().local_minus_utc(), 3600);
        assert_eq!(parse_offset("Z").unwrap().local_minus_utc(), 0);
        for bad in ["5", "-05:60", "+15:00", "-5:00", "EST"] {
            assert!(parse_offset(bad).is_err(), "{bad}");
        }
        assert!(parse_zone("America/New_York").is_ok());
        assert!(parse_zone("Mars/Olympus").is_err());
        assert_eq!(
            parse_shift("+1y").unwrap(),
            Shift {
                months: 12,
                seconds: 0
            }
        );
        assert_eq!(
            parse_shift("-1y2mo").unwrap(),
            Shift {
                months: -14,
                seconds: 0
            }
        );
        assert_eq!(
            parse_shift("1d12h").unwrap(),
            Shift {
                months: 0,
                seconds: 129_600
            }
        );
        assert_eq!(
            parse_shift("-30m").unwrap(),
            Shift {
                months: 0,
                seconds: -1800
            }
        );
        assert_eq!(parse_shift("+2w5s").unwrap().seconds, 14 * 86_400 + 5);
        for bad in ["", "+", "1", "y", "+1x", "+0d", "1.5h", "+1y-2d"] {
            assert!(parse_shift(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn set_keeps_each_items_offset_unless_a_zone_is_given() {
        let items = [
            item("a", 0, Some(EST)),
            item("b", 0, Some(3_600_000)),
            item("c", 0, None),
        ];
        let local = at("2026-11-22T10:00");
        let kept = plan(&items, &Plan::Set(local), Zone::Keep).unwrap();
        assert_eq!(kept[0].taken_at(), "2026-11-22T10:00:00-05:00");
        assert_eq!(kept[1].taken_at(), "2026-11-22T10:00:00+01:00");
        assert_eq!(kept[2].taken_at(), "2026-11-22T10:00:00+00:00");
        let fixed = plan(
            &items,
            &Plan::Set(local),
            Zone::Fixed(parse_offset("+09:00").unwrap()),
        )
        .unwrap();
        assert!(fixed
            .iter()
            .all(|t| t.taken_at() == "2026-11-22T10:00:00+09:00"));
    }

    #[test]
    fn named_zones_follow_daylight_saving() {
        let tz = Zone::Named(parse_zone("America/New_York").unwrap());
        let items = [item("a", 0, Some(0))];
        let winter = plan(&items, &Plan::Set(at("2026-01-10T12:00")), tz).unwrap();
        assert_eq!(winter[0].taken_at(), "2026-01-10T12:00:00-05:00");
        let summer = plan(&items, &Plan::Set(at("2026-07-10T12:00")), tz).unwrap();
        assert_eq!(summer[0].taken_at(), "2026-07-10T12:00:00-04:00");
        // 02:30 on the spring-forward day does not exist.
        assert!(plan(&items, &Plan::Set(at("2026-03-08T02:30")), tz).is_err());
    }

    #[test]
    fn keep_spacing_moves_the_earliest_to_the_date_and_keeps_gaps() {
        // Like the album: three photos a second apart, listed out of order.
        let base = 1_764_460_470_000; // 2025-11-29T18:54:30-05:00
        let items = [
            item("b", base + 1000, Some(EST)),
            item("a", base, Some(EST)),
            item("c", base + 2000, Some(EST)),
        ];
        let out = plan(
            &items,
            &Plan::KeepSpacing(at("2026-11-22T10:00")),
            Zone::Keep,
        )
        .unwrap();
        assert_eq!(out[1].taken_at(), "2026-11-22T10:00:00-05:00");
        assert_eq!(out[0].taken_at(), "2026-11-22T10:00:01-05:00");
        assert_eq!(out[2].taken_at(), "2026-11-22T10:00:02-05:00");
        // A zone change across DST keeps real elapsed time; offsets follow the zone.
        let ny = Zone::Named(parse_zone("America/New_York").unwrap());
        let day = 86_400_000;
        let items = [item("x", 0, Some(0)), item("y", 3 * day, Some(0))];
        let out = plan(&items, &Plan::KeepSpacing(at("2026-03-06T12:00")), ny).unwrap();
        assert_eq!(out[0].taken_at(), "2026-03-06T12:00:00-05:00");
        assert_eq!(out[1].taken_at(), "2026-03-09T13:00:00-04:00");
        assert!(plan(&[], &Plan::KeepSpacing(at("2026-01-01")), Zone::Keep)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn shift_moves_wall_clock_and_keeps_offset() {
        let base = 1_764_460_470_000;
        let items = [item("a", base, Some(EST))];
        let year = plan(
            &items,
            &Plan::Shift(parse_shift("+1y").unwrap()),
            Zone::Keep,
        )
        .unwrap();
        assert_eq!(year[0].taken_at(), "2026-11-29T18:54:30-05:00");
        let back = plan(
            &items,
            &Plan::Shift(parse_shift("-1d2h").unwrap()),
            Zone::Keep,
        )
        .unwrap();
        assert_eq!(back[0].taken_at(), "2025-11-28T16:54:30-05:00");
        // Jan 31 + 1 month clamps to the end of February.
        let jan = item("j", 1_769_860_800_000, Some(0)); // 2026-01-31T12:00Z
        let feb = plan(
            &[jan],
            &Plan::Shift(parse_shift("+1mo").unwrap()),
            Zone::Keep,
        )
        .unwrap();
        assert_eq!(feb[0].taken_at(), "2026-02-28T12:00:00+00:00");
    }

    #[test]
    fn restoring_the_original_matches_and_payload_is_in_seconds() {
        let original = item("a", 1_764_460_470_000, Some(EST));
        let (local, offset) = parse_date("2025-11-29T18:54:30-05:00").unwrap();
        let t = plan(
            std::slice::from_ref(&original),
            &Plan::Set(local),
            Zone::Fixed(offset.unwrap()),
        )
        .unwrap()[0];
        assert_eq!(
            t,
            Target {
                seconds: 1_764_460_470,
                offset_seconds: -18000
            }
        );
        assert!(t.matches(&original));
        assert!(!t.matches(&item("a", 1_764_460_470_000, None)));
        assert_eq!(
            write_payload(&[(&original, t)]).to_string(),
            r#"[[["dedup-a",1764460470,-18000]]]"#
        );
    }

    #[test]
    fn parses_extended_records_and_album_pages() {
        let mut record = vec![Value::Null; 12];
        record[0] = json!("AF1QipSynthetic001");
        record[2] = json!("IMG_1.jpg");
        record[3] = json!(1_764_460_470_000i64);
        record[4] = json!(EST);
        record[11] = json!("Dedup_Key-1");
        let parsed = parse_dated(&json!([record]), "AF1QipRequested").unwrap();
        assert_eq!(parsed.id, "AF1QipRequested");
        assert_eq!(parsed.dedup_key, "Dedup_Key-1");
        assert_eq!(parsed.filename.as_deref(), Some("IMG_1.jpg"));
        assert_eq!(parsed.taken_at(), "2025-11-29T18:54:30-05:00");
        let mut no_key = record.clone();
        no_key[11] = json!("../x");
        assert!(parse_dated(&json!([no_key]), "AF1QipRequested").is_err());
        assert!(parse_dated(&Value::Null, "AF1QipRequested").is_err());

        let page = json!([
            null,
            [
                ["AF1QipItemOne1", [], 1, "d"],
                ["../bad"],
                ["AF1QipItemTwo2"]
            ],
            "next",
            ["album"]
        ]);
        let (ids, next) = parse_album_page(&page).unwrap();
        assert_eq!(ids, vec!["AF1QipItemOne1", "AF1QipItemTwo2"]);
        assert_eq!(next.as_deref(), Some("next"));
        let (_, last) = parse_album_page(&json!([null, [], ""])).unwrap();
        assert!(last.is_none());
        assert!(parse_album_page(&Value::Null).is_err());
        assert_eq!(
            album_page_payload("AF1QipAlbum", None).to_string(),
            r#"["AF1QipAlbum",null,null,null]"#
        );
    }
}
