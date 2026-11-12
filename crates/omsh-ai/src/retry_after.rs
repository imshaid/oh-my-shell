//! The `Retry-After` header of a failed HTTP answer: how long the server asks us to wait.
//!
//! Two headers carry it: `retry-after-ms` (milliseconds; some providers send it) and
//! `retry-after`, which is either a number of seconds or an HTTP date
//! (`Sun, 06 Nov 1994 08:49:37 GMT`). The reader here takes a function that looks a header up,
//! so it does not depend on one HTTP crate's types and can be tested with plain text.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A header value larger than this is cut to it. It keeps the arithmetic far from overflow;
/// the retry code decides separately whether a wait is short enough to be worth it.
const LARGEST: Duration = Duration::from_secs(24 * 60 * 60);

/// How long the answer asks to wait, if it says. `now` is the time to measure an HTTP date from.
/// A date that has passed, a negative number and anything that is not a number or a date give
/// `None`, as does a header that is not there.
pub(crate) fn from_headers(
    get: impl Fn(&str) -> Option<String>,
    now: SystemTime,
) -> Option<Duration> {
    if let Some(ms) = get("retry-after-ms").and_then(|v| digits(&v)) {
        return Some(Duration::from_millis(ms).min(LARGEST));
    }
    let value = get("retry-after")?;
    let value = value.trim();
    if let Some(seconds) = digits(value) {
        return Some(Duration::from_secs(seconds).min(LARGEST));
    }
    let at = http_date(value)?;
    let wait = at.duration_since(now).ok()?;
    Some(wait.min(LARGEST))
}

/// A whole number written with digits only (no sign, no fraction, no unit).
fn digits(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Too many digits for a `u64` means "very long"; the cap above turns that into a day.
    Some(text.parse().unwrap_or(u64::MAX / 2_000))
}

/// `Sun, 06 Nov 1994 08:49:37 GMT` (the form HTTP/1.1 asks senders to use).
fn http_date(text: &str) -> Option<SystemTime> {
    let mut words = text.split_whitespace();
    let weekday = words.next()?;
    if !weekday.ends_with(',') {
        return None;
    }
    let day: i64 = words.next()?.parse().ok()?;
    let month = match words.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = words.next()?.parse().ok()?;
    let mut clock = words.next()?.split(':');
    let hour: u64 = clock.next()?.parse().ok()?;
    let minute: u64 = clock.next()?.parse().ok()?;
    let second: u64 = clock.next()?.parse().ok()?;
    if clock.next().is_some() || words.next()? != "GMT" || words.next().is_some() {
        return None;
    }
    if !(1970..=9999).contains(&year) || !(1..=days_in(year, month)).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = u64::try_from(days_from_civil(year, month, day)).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(days * 86_400 + hour * 3600 + minute * 60 + second))
}

fn days_in(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the given day (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(headers: &[(&str, &str)], now: SystemTime) -> Option<Duration> {
        from_headers(
            |name| {
                headers
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            },
            now,
        )
    }

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn seconds_and_milliseconds_are_read() {
        assert_eq!(
            read(&[("retry-after", "3")], at(0)),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            read(&[("retry-after", " 12 ")], at(0)),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            read(&[("retry-after-ms", "250")], at(0)),
            Some(Duration::from_millis(250))
        );
        // Both: the one with milliseconds is the more exact.
        assert_eq!(
            read(&[("retry-after", "9"), ("retry-after-ms", "1500")], at(0)),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(read(&[("retry-after", "0")], at(0)), Some(Duration::ZERO));
    }

    #[test]
    fn an_http_date_is_measured_from_now() {
        // 1994-11-06 08:49:37 GMT is 784111777 seconds after the epoch.
        let header = [("retry-after", "Sun, 06 Nov 1994 08:49:37 GMT")];
        assert_eq!(
            read(&header, at(784_111_777 - 30)),
            Some(Duration::from_secs(30))
        );
        assert_eq!(read(&header, at(784_111_777)), Some(Duration::ZERO));
        // Already past: no extra wait is asked.
        assert_eq!(read(&header, at(784_111_777 + 1)), None);
        // A leap day.
        let leap = [("retry-after", "Thu, 29 Feb 2024 00:00:00 GMT")];
        assert_eq!(
            read(&leap, at(1_709_164_800 - 5)),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn nonsense_is_ignored() {
        for value in [
            "",
            "soon",
            "-5",
            "1.5",
            "5s",
            "+5",
            "0x10",
            "Sun, 06 Nov 1994 08:49:37 UTC",
            "Sun, 31 Apr 1994 08:49:37 GMT",
            "Sun, 29 Feb 2023 08:49:37 GMT",
            "Sun, 06 Nov 1969 08:49:37 GMT",
            "Sun, 06 Nov 1994 25:49:37 GMT",
            "Sun, 06 Nov 1994 08:49 GMT",
            "Sun, 06 Nov 1994 08:49:37 GMT extra",
            "06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
        ] {
            assert_eq!(read(&[("retry-after", value)], at(0)), None, "{value:?}");
        }
        assert_eq!(read(&[("retry-after-ms", "abc")], at(0)), None);
        assert_eq!(read(&[], at(0)), None);
    }

    #[test]
    fn a_huge_value_is_cut_to_a_day_and_does_not_overflow() {
        let day = Some(Duration::from_secs(86_400));
        assert_eq!(
            read(&[("retry-after", "99999999999999999999999999")], at(0)),
            day
        );
        assert_eq!(read(&[("retry-after", "18446744073709551615")], at(0)), day);
        assert_eq!(
            read(&[("retry-after-ms", "18446744073709551615")], at(0)),
            day
        );
        assert_eq!(
            read(&[("retry-after", "Fri, 31 Dec 9999 23:59:59 GMT")], at(0)),
            day
        );
    }
}
