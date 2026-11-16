//! Dates and times as the Calendar API writes them (RFC 3339) and as unix seconds, in UTC; the
//! days of the proleptic Gregorian calendar (Howard Hinnant's `days_from_civil`).

/// Seconds of a day.
pub const DAY: i64 = 86_400;

/// "2026-11-16T09:30:00Z" of a unix time.
pub fn rfc3339_utc(unix: i64) -> String {
    let (y, m, d) = civil_from_days(unix.div_euclid(DAY));
    let s = unix.rem_euclid(DAY);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s / 60 % 60, s % 60)
}

/// The unix time of 00:00 UTC of a date ("2026-11-16").
pub fn parse_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, m, d): (i64, u32, u32) = (number(s.get(0..4)?)?, number(s.get(5..7)?)?, number(s.get(8..10)?)?);
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d) * DAY)
}

/// The unix time of an RFC 3339 date-time ("2026-11-16T10:00:00+01:00", "...Z", with or without
/// fractions of a second, which are dropped).
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let date = parse_date(s.get(0..10)?)?;
    let b = s.as_bytes();
    if b.len() < 20 || !matches!(b[10], b'T' | b't') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (h, mi, se): (i64, i64, i64) = (number(s.get(11..13)?)?, number(s.get(14..16)?)?, number(s.get(17..19)?)?);
    // 60: a leap second, taken as the last second of its minute
    if h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = s.get(19..)?;
    if let Some(fraction) = rest.strip_prefix('.') {
        let n = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        rest = &fraction[n..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let o = rest.as_bytes();
            if o.len() != 6 || o[3] != b':' {
                return None;
            }
            let sign = match o[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (oh, om): (i64, i64) = (number(rest.get(1..3)?)?, number(rest.get(4..6)?)?);
            if oh > 23 || om > 59 {
                return None;
            }
            sign * (oh * 3600 + om * 60)
        }
    };
    Some(date + h * 3600 + mi * 60 + se.min(59) - offset)
}

/// Digits only (no sign, no space), as a number.
fn number<T: std::str::FromStr>(s: &str) -> Option<T> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

/// Days from 1970-01-01 to a date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let (m, d) = (i64::from(m), i64::from(d));
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day counted from 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_written_in_utc() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_763_285_400), "2025-11-16T09:30:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn dates_are_midnight_utc() {
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(parse_date("2000-02-29"), Some(951_782_400));
        assert_eq!(parse_date("2026-11-17").unwrap() - parse_date("2026-11-16").unwrap(), DAY);
        for bad in ["2026-02-29", "2026-13-01", "2026-00-10", "2026-1-01", "2026/11/16", "+026-11-16", ""] {
            assert_eq!(parse_date(bad), None, "{bad}");
        }
    }

    #[test]
    fn date_times_with_offsets() {
        let utc = parse_rfc3339("2025-11-16T09:30:00Z").unwrap();
        assert_eq!(utc, 1_763_285_400);
        assert_eq!(parse_rfc3339("2025-11-16T10:30:00+01:00"), Some(utc));
        assert_eq!(parse_rfc3339("2025-11-16T04:00:00-05:30"), Some(utc));
        assert_eq!(parse_rfc3339("2025-11-16T09:30:00.123Z"), Some(utc));
        // a time zone's offset may move the date
        assert_eq!(parse_rfc3339("2025-11-17T00:30:00+15:00"), Some(utc));
        for bad in ["2025-11-16T09:30:00", "2025-11-16 09:30:00Z", "2025-11-16T24:00:00Z", "2025-11-16T09:30:00+0100", "2025-11-16T09:30:00.Z", "2025-11-16Tä9:30:00Z"] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }
}
