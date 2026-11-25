//! .NET TimeSpan "c" format: `[-][d.]hh:mm:ss[.fffffff]` (1 tick = 100 ns).

use std::time::Duration;

/// Parse a TimeSpan text. Negative values give zero.
pub fn parse(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.starts_with('-') {
        return Some(Duration::ZERO);
    }
    let mut parts = s.split(':');
    let first = parts.next()?;
    let mm = parts.next();
    let ss = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let (days, hours) = match first.split_once('.') {
        Some((d, h)) => (d.parse::<u64>().ok()?, h.parse::<u64>().ok()?),
        None if mm.is_none() => (first.parse::<u64>().ok()?, 0), // "d" alone
        None => (0, first.parse::<u64>().ok()?),
    };
    let mins = match mm {
        Some(m) => m.parse::<u64>().ok()?,
        None => 0,
    };
    let (secs, nanos) = match ss {
        None => (0, 0),
        Some(ss) => match ss.split_once('.') {
            None => (ss.parse::<u64>().ok()?, 0),
            Some((sec, frac)) => {
                if frac.is_empty() || frac.len() > 7 || !frac.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let ticks: u64 = format!("{frac:0<7}").parse().ok()?;
                (sec.parse::<u64>().ok()?, (ticks * 100) as u32)
            }
        },
    };
    let total = days
        .checked_mul(86_400)?
        .checked_add(hours.checked_mul(3600)?)?
        .checked_add(mins.checked_mul(60)?)?
        .checked_add(secs)?;
    Some(Duration::new(total, nanos))
}

/// Format as TimeSpan "c": `1.02:03:04.5000000`, `00:00:00`.
pub fn format(d: Duration) -> String {
    let total = d.as_secs();
    let days = total / 86_400;
    let rem = total % 86_400;
    let mut s = String::new();
    if days > 0 {
        s.push_str(&format!("{days}."));
    }
    s.push_str(&format!("{:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60));
    let ticks = d.subsec_nanos() / 100;
    if ticks > 0 {
        s.push_str(&format!(".{ticks:07}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let d = parse("1.02:03:04.5000000").unwrap();
        assert_eq!(d, Duration::new(86_400 + 2 * 3600 + 3 * 60 + 4, 500_000_000));
        assert_eq!(format(d), "1.02:03:04.5000000");
        assert_eq!(parse("00:00:00").unwrap(), Duration::ZERO);
        assert_eq!(format(Duration::ZERO), "00:00:00");
        assert_eq!(parse("12:30:00").unwrap(), Duration::from_secs(45_000));
        assert_eq!(parse("00:00:01.25").unwrap(), Duration::from_millis(1250));
        assert_eq!(format(Duration::from_secs(3 * 86_400 + 1)), "3.00:00:01");
        assert!(parse("garbage").is_none());
        assert!(parse("1:2:3:4").is_none());
    }
}
