//! Board timestamps (RFC 3339, as Paperclip's API serialises them) to the herald's `Instant`:
//! nanoseconds since the Unix epoch. In Watch mode the board's own times are the evidence, so the
//! herald's clocks run on them; a live watcher passes the wall time in the same unit.

use runo_core::time::Instant;

/// Parse `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`; `None` for anything else.
#[must_use]
#[expect(
    clippy::many_single_char_names,
    reason = "the date and time fields, read in RFC 3339 order"
)]
pub fn parse(s: &str) -> Option<Instant> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if b[13] != b':' || b[16] != b':' || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut nanos: i64 = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let digits = s.get(start..i.min(start + 9))?;
        if digits.is_empty() {
            return None;
        }
        nanos = digits.parse::<i64>().ok()? * 10_i64.pow(u32::try_from(9 - digits.len()).ok()?);
    }
    let offset = match s.get(i..)? {
        "Z" | "z" => 0,
        tz if tz.len() == 6
            && matches!(tz.as_bytes()[0], b'+' | b'-')
            && tz.as_bytes()[3] == b':' =>
        {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            sign * (num(i + 1..i + 3)? * 3600 + num(i + 4..i + 6)? * 60)
        }
        _ => return None,
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset;
    let total = i128::from(secs) * 1_000_000_000 + i128::from(nanos);
    u64::try_from(total).ok().map(Instant)
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::parse;
    use runo_core::time::Instant;

    #[test]
    fn known_instants() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(Instant(0)));
        assert_eq!(
            parse("2026-11-03T16:58:58.913Z"),
            Some(Instant(1_793_725_138_913_000_000))
        );
        assert_eq!(
            parse("2026-11-03T18:58:58.913+02:00"),
            parse("2026-11-03T16:58:58.913Z"),
            "an offset names the same instant"
        );
        assert_eq!(
            parse("2024-02-29T12:00:00.123456789123Z").map(|i| i.0 % 1_000_000_000),
            Some(123_456_789)
        );
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        for bad in [
            "",
            "2026-11-03",
            "2026-13-01T00:00:00Z",
            "2026-11-03T16:58:58",
            "1969-12-31T23:59:59Z",
            "2026-11-03T16:58:58.Z",
            "2026-11-03T1a:58:58Z",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }
}
