use crate::civil::{civil_from_days, Date};

const MAX_SECONDS: i64 = 253_402_300_799;
const MAX_FRACTION: usize = 9;

fn digits(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn offset(zone: &str) -> Option<i64> {
    if zone == "Z" {
        return Some(0);
    }
    let sign = match zone.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    if zone.len() != 6 || zone.get(3..4) != Some(":") {
        return None;
    }
    let hours = digits(zone.get(1..3)?)?;
    let minutes = digits(zone.get(4..6)?)?;
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 3600 + minutes * 60))
}

pub(crate) fn parse(text: &str) -> Option<i64> {
    let (day, rest) = text.split_once('T')?;
    let day = Date::parse(day)?;
    if rest.get(2..3) != Some(":") || rest.get(5..6) != Some(":") {
        return None;
    }
    let hour = digits(rest.get(0..2)?)?;
    let minute = digits(rest.get(3..5)?)?;
    let second = digits(rest.get(6..8)?)?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut zone = rest.get(8..)?;
    if let Some(fraction) = zone.strip_prefix('.') {
        let length = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if !(1..=MAX_FRACTION).contains(&length) {
            return None;
        }
        zone = fraction.get(length..)?;
    }
    let local = day
        .days()
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?;
    local.checked_sub(offset(zone)?)
}

pub(crate) fn iso(seconds: i64) -> String {
    let seconds = seconds.clamp(0, MAX_SECONDS);
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let rest = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_timestamps_are_read_as_utc() {
        assert_eq!(parse("2026-11-13T14:09:25Z"), Some(1_794_578_965));
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse("2000-02-29T23:59:59Z"), Some(951_868_799));
        assert_eq!(parse("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(parse("1969-12-31T23:59:59Z"), Some(-1));
        assert_eq!(parse("9999-12-31T23:59:59Z"), Some(MAX_SECONDS));
    }

    #[test]
    fn fractions_are_ignored_and_offsets_are_applied() {
        assert_eq!(parse("2026-11-13T14:09:25.1Z"), Some(1_794_578_965));
        assert_eq!(parse("2026-11-13T14:09:25.1234567Z"), Some(1_794_578_965));
        assert_eq!(parse("2026-11-13T14:09:25.123456789Z"), Some(1_794_578_965));
        assert_eq!(parse("2026-11-13T15:09:25+01:00"), Some(1_794_578_965));
        assert_eq!(parse("2026-11-13T09:09:25-05:00"), Some(1_794_578_965));
        assert_eq!(parse("2026-11-13T14:09:25.5+00:00"), Some(1_794_578_965));
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            "",
            "2026-11-13",
            "2026-11-13T14:09:25",
            "2026-11-13T14:09:25z",
            "2026-11-13 14:09:25Z",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-11-13T24:00:00Z",
            "2026-11-13T14:60:00Z",
            "2026-11-13T14:09:60Z",
            "2026-11-13T14:09Z",
            "2026-11-13T14-09-25Z",
            "2026-11-13T14:09:25.Z",
            "2026-11-13T14:09:25.1234567890Z",
            "2026-11-13T14:09:25+0100",
            "2026-11-13T14:09:25+24:00",
            "2026-11-13T14:09:25+01:60",
            "2026-11-13T14:09:25*01:00",
            "2026-11-13T14:09:25Zjunk",
            "2026-11-13T14:09:25Z\n",
            "x2026-11-13T14:09:25Z",
            "2026-11-13T14:09:2\u{e4}Z",
            "\u{1d7d9}026-11-13T14:09:25Z",
            "T",
            "TT",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn instants_are_written_for_filters() {
        assert_eq!(iso(1_794_578_965), "2026-11-13T14:09:25Z");
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(951_868_799), "2000-02-29T23:59:59Z");
        assert_eq!(iso(-5), "1970-01-01T00:00:00Z");
        assert_eq!(iso(i64::MAX), "9999-12-31T23:59:59Z");
        assert_eq!(parse(&iso(1_794_319_765)), Some(1_794_319_765));
    }

    #[test]
    fn mutated_timestamps_never_panic() {
        let seed = "2026-11-13T14:09:25.123456+01:30";
        for end in 0..=seed.len() {
            if let Some(prefix) = seed.get(..end) {
                let _ = parse(prefix);
            }
        }
        for (position, _) in seed.char_indices() {
            for replacement in [
                "", "9", "-", ":", "T", "Z", ".", "+", " ", "\u{e4}", "\u{0}",
            ] {
                let mut mutated = String::new();
                for (index, c) in seed.char_indices() {
                    if index == position {
                        mutated.push_str(replacement);
                    } else {
                        mutated.push(c);
                    }
                }
                let _ = parse(&mutated);
            }
        }
        assert_eq!(parse(seed), Some(1_794_578_965 - 5400));
    }
}
