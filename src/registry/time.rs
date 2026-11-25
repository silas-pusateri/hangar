//! Date helpers for registry.json `install_time` (Newtonsoft round-trip form) and the
//! local time stamp used when a corrupt registry is renamed.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::util::Utc;

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Newtonsoft UTC form: `yyyy-MM-ddTHH:mm:ss.FFFFFFFZ` (trailing fraction zeros trimmed).
pub fn format_utc(t: &Utc) -> String {
    let mut s = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.min, t.sec
    );
    let frac = format!("{:07}", t.nanos / 100);
    let frac = frac.trim_end_matches('0');
    if !frac.is_empty() {
        s.push('.');
        s.push_str(frac);
    }
    s.push('Z');
    s
}

/// Current time in the registry form.
pub fn now_string() -> String {
    format_utc(&Utc::now())
}

fn num(s: &str, from: usize, len: usize) -> Option<u32> {
    let part = s.get(from..from + len)?;
    if !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// Parse an ISO-8601 date and time with an optional fraction and zone (`Z`, `+11:00`, `-0500`,
/// or none = UTC, as Newtonsoft does with DateTimeZoneHandling.Utc). Returns the UTC form, or
/// None if the text is not understood (the caller keeps the original text).
pub fn normalize_to_utc(s: &str) -> Option<String> {
    let s = s.trim();
    let year = num(s, 0, 4)?;
    let month = num(s, 5, 2)?;
    let day = num(s, 8, 2)?;
    let hour = num(s, 11, 2)?;
    let min = num(s, 14, 2)?;
    let sec = num(s, 17, 2)?;
    let sep_ok = s.get(4..5) == Some("-")
        && s.get(7..8) == Some("-")
        && matches!(s.get(10..11), Some("T") | Some("t") | Some(" "))
        && s.get(13..14) == Some(":")
        && s.get(16..17) == Some(":");
    if !sep_ok || !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let mut rest = s.get(19..)?;
    let mut nanos: u32 = 0;
    if let Some(r) = rest.strip_prefix('.') {
        let digits: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        let mut padded: String = digits.chars().take(9).collect();
        while padded.len() < 9 {
            padded.push('0');
        }
        nanos = padded.parse().ok()?;
        rest = r.get(digits.len()..)?;
    }
    let offset_secs: i64 = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match rest.get(0..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            let body = rest.get(1..)?;
            let (h, m) = match body.len() {
                5 if body.get(2..3) == Some(":") => (num(body, 0, 2)?, num(body, 3, 2)?),
                4 => (num(body, 0, 2)?, num(body, 2, 2)?),
                2 => (num(body, 0, 2)?, 0),
                _ => return None,
            };
            sign * (h as i64 * 3600 + m as i64 * 60)
        }
    };
    let local = days_from_civil(year as i64, month, day) * 86_400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
    let total = local - offset_secs;
    if total < 0 {
        return None;
    }
    let t = Utc::from_system(UNIX_EPOCH + Duration::new(total as u64, nanos));
    Some(format_utc(&t))
}

/// Local broken-down time now: (year, month, day, hour, min, sec). Falls back to UTC.
fn local_now() -> (i64, u32, u32, u32, u32, u32) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let now = secs as libc::time_t;
    // SAFETY: localtime_r only writes into the tm we own; a zeroed tm is a valid value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&now, &mut tm).is_null() };
    if ok {
        let u = |v: libc::c_int| u32::try_from(v).unwrap_or(0);
        (tm.tm_year as i64 + 1900, u(tm.tm_mon) + 1, u(tm.tm_mday), u(tm.tm_hour), u(tm.tm_min), u(tm.tm_sec))
    } else {
        let u = Utc::now();
        (u.year, u.month, u.day, u.hour, u.min, u.sec)
    }
}

/// Local time as `yyyyMMddHHmmss` (CKAN's corrupt-registry suffix).
pub fn local_compact_stamp() -> String {
    let (y, mo, d, h, mi, s) = local_now();
    format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}")
}

/// Local time as `yyyy-MM-dd_HH-mm-ss` (CKAN history snapshot names use DateTime.Now).
pub fn local_file_stamp() -> String {
    let (y, mo, d, h, mi, s) = local_now();
    format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_become_utc() {
        assert_eq!(normalize_to_utc("2014-11-14T16:47:59.774894+11:00").as_deref(), Some("2014-11-14T05:47:59.774894Z"));
        assert_eq!(normalize_to_utc("2025-03-01T12:34:56.1234567Z").as_deref(), Some("2025-03-01T12:34:56.1234567Z"));
        assert_eq!(normalize_to_utc("2025-03-01T12:00:00Z").as_deref(), Some("2025-03-01T12:00:00Z"));
        assert_eq!(normalize_to_utc("2025-03-01T12:00:00.5000000Z").as_deref(), Some("2025-03-01T12:00:00.5Z"));
        assert_eq!(normalize_to_utc("2025-01-01T01:00:00+02:00").as_deref(), Some("2024-12-31T23:00:00Z"));
        assert_eq!(normalize_to_utc("2025-01-01T23:30:00-0130").as_deref(), Some("2025-01-02T01:00:00Z"));
        assert_eq!(normalize_to_utc("2025-01-01T23:30:00").as_deref(), Some("2025-01-01T23:30:00Z"));
        assert_eq!(normalize_to_utc("garbage"), None);
        assert_eq!(normalize_to_utc("2025-13-01T00:00:00Z"), None);
        assert_eq!(normalize_to_utc("2025-01-01T00:00:00+1"), None);
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(local_compact_stamp().len(), 14);
        let f = local_file_stamp();
        assert_eq!(f.len(), 19);
        assert_eq!(f.get(10..11), Some("_"));
    }
}
