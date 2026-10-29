use std::time::{SystemTime, UNIX_EPOCH};

/// Parses the RFC 3339 UTC timestamps `gh` returns (`2026-10-29T07:37:54Z`).
/// Diffy only needs second precision to say how old a request or review is.
pub fn parse_utc(value: &str) -> Option<i64> {
    if value.len() < 19 {
        return None;
    }
    let part = |range: std::ops::Range<usize>| value.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (part(0..4)?, part(5..7)?, part(8..10)?);
    let (hour, minute, second) = (part(11..13)?, part(14..16)?, part(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Howard Hinnant's days-from-civil, valid for the Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// Short age for a list row: `just now`, `4m ago`, `3h ago`, `2d ago`.
pub fn ago(epoch: i64, now: i64) -> String {
    let seconds = now.saturating_sub(epoch);
    let (count, unit) = match seconds {
        seconds if seconds < 60 => return "just now".to_string(),
        seconds if seconds < 3_600 => (seconds / 60, "m"),
        seconds if seconds < 86_400 => (seconds / 3_600, "h"),
        seconds if seconds < 7 * 86_400 => (seconds / 86_400, "d"),
        seconds if seconds < 30 * 86_400 => (seconds / (7 * 86_400), "w"),
        seconds if seconds < 365 * 86_400 => (seconds / (30 * 86_400), "mo"),
        seconds => (seconds / (365 * 86_400), "y"),
    };
    format!("{count}{unit} ago")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gh_timestamps_as_utc_seconds() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2026-10-29T07:37:54Z"), Some(1_793_259_474));
        assert_eq!(parse_utc(""), None);
        assert_eq!(parse_utc("not a date"), None);
        assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn ages_read_like_a_queue() {
        let now = 1_000_000_000;
        assert_eq!(ago(now - 10, now), "just now");
        assert_eq!(ago(now - 4 * 60, now), "4m ago");
        assert_eq!(ago(now - 3 * 3_600, now), "3h ago");
        assert_eq!(ago(now - 2 * 86_400, now), "2d ago");
        assert_eq!(ago(now - 21 * 86_400, now), "3w ago");
        assert_eq!(ago(now - 120 * 86_400, now), "4mo ago");
        assert_eq!(ago(now + 60, now), "just now", "future clocks do not panic");
    }
}
