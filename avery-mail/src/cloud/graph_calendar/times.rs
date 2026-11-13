use crate::civil::{Date, DateTime};
use crate::json::Json;

const MAX_ZONE: usize = 64;
const MAX_STAMP: usize = 40;
const SAME_DAY_UNTIL: i64 = 10 * 60;
const UTC_NAMES: [&str; 2] = ["UTC", "Etc/UTC"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    pub(super) at: DateTime,
    in_zone: bool,
    in_utc: bool,
}

impl Stamp {
    pub(super) fn timed(self) -> Option<DateTime> {
        self.in_zone.then_some(self.at)
    }

    pub(super) fn day(self) -> Option<Date> {
        let date = self.at.date();
        if self.at.minute_of_day() == 0 {
            return Some(date);
        }
        if !self.in_utc {
            return None;
        }
        if self.at.minute_of_day() <= SAME_DAY_UNTIL {
            Some(date)
        } else {
            date.add_days(1)
        }
    }

    pub(super) fn utc_day(self) -> Option<Date> {
        if self.in_utc {
            self.day()
        } else {
            None
        }
    }
}

pub(super) fn zone_ok(zone: &str) -> bool {
    !zone.is_empty()
        && zone.len() <= MAX_ZONE
        && !zone.starts_with('/')
        && !zone.contains("..")
        && zone
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_+-/".contains(c))
}

fn is_utc(name: &str) -> bool {
    UTC_NAMES.iter().any(|utc| utc.eq_ignore_ascii_case(name))
}

fn same_zone(label: &str, zone: &str) -> bool {
    label.eq_ignore_ascii_case(zone) || (is_utc(label) && is_utc(zone))
}

fn clock_tail(tail: &str) -> bool {
    if tail.is_empty() {
        return true;
    }
    let Some(seconds) = tail.strip_prefix(':') else {
        return false;
    };
    let (whole, fraction) = match seconds.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (seconds, None),
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    whole.len() == 2
        && digits(whole)
        && whole.parse::<u8>().is_ok_and(|seconds| seconds <= 60)
        && fraction.is_none_or(|fraction| digits(fraction) && fraction.len() <= 9)
}

pub(super) fn moment(text: &str) -> Option<DateTime> {
    if text.len() > MAX_STAMP {
        return None;
    }
    let head = text.get(..16)?;
    if head.as_bytes().get(10) != Some(&b'T') || !clock_tail(text.get(16..)?) {
        return None;
    }
    DateTime::parse(head)
}

pub(super) fn stamp(value: Option<&Json>, zone: &str) -> Option<Stamp> {
    let object = value?.as_object()?;
    let at = moment(object.get("dateTime")?.as_str()?)?;
    let label = object.get("timeZone")?.as_str()?;
    Some(Stamp {
        at,
        in_zone: same_zone(label, zone),
        in_utc: is_utc(label),
    })
}

pub(super) fn local_stamp(at: DateTime) -> String {
    format!("{}T{}:00", at.date(), at.clock())
}

pub(super) fn instant(at: DateTime) -> String {
    format!("{}Z", local_stamp(at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{j, TestResult};

    fn stamp_of(
        date_time: &str,
        label: &str,
        zone: &str,
    ) -> Result<Stamp, Box<dyn std::error::Error>> {
        let value = j(&format!(
            r#"{{"dateTime": "{date_time}", "timeZone": "{label}"}}"#
        ))?;
        Ok(stamp(Some(&value), zone).ok_or("stamp")?)
    }

    fn date(text: &str) -> Result<Date, Box<dyn std::error::Error>> {
        Ok(Date::parse(text).ok_or("date")?)
    }

    #[test]
    fn zones_are_checked_before_they_reach_a_header_or_a_body() {
        for good in [
            "Europe/Zurich",
            "America/Argentina/Buenos_Aires",
            "UTC",
            "Etc/GMT+1",
            "America/Port-au-Prince",
        ] {
            assert!(zone_ok(good), "{good}");
        }
        let long = "a".repeat(MAX_ZONE + 1);
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "Europe/Zu rich",
            "Europe/Zurich\"",
            "Europe/Zurich\r\nX: y",
            "Europe/Z\u{fc}rich",
            "a,b",
            "a;b",
            long.as_str(),
        ] {
            assert!(!zone_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn moments_have_minute_precision_and_a_strict_shape() -> TestResult {
        let at = |text: &str| moment(text).map(|m| m.to_string());
        assert_eq!(
            at("2026-11-20T09:30:00.0000000"),
            Some("2026-11-20 09:30".into())
        );
        assert_eq!(at("2026-11-20T09:30:59"), Some("2026-11-20 09:30".into()));
        assert_eq!(at("2026-11-20T09:30"), Some("2026-11-20 09:30".into()));
        assert_eq!(at("2026-11-20T23:59:60.1"), Some("2026-11-20 23:59".into()));
        assert_eq!(
            at("2026-11-20T00:00:00.123456789"),
            Some("2026-11-20 00:00".into())
        );
        for bad in [
            "",
            "2026-11-20",
            "2026-11-20 09:30:00",
            "2026-11-20T09:30:00Z",
            "2026-11-20T09:30:00+01:00",
            "2026-11-20T09:30:00.",
            "2026-11-20T09:30:00.1234567890",
            "2026-11-20T09:30:61",
            "2026-11-20T09:30:5",
            "2026-11-20T09:30:0a",
            "2026-11-20T09:30:00 ",
            "2026-11-20T09:30x",
            "2026-13-20T09:30:00",
            "2026-11-20T24:00:00",
            "2026-02-30T09:30:00",
            "\u{ff12}026-11-20T09:30:00",
            "2026-11-20T09:3\u{e4}:00",
        ] {
            assert_eq!(at(bad), None, "{bad:?}");
        }
        let long = format!("2026-11-20T09:30:00.{}", "0".repeat(60));
        assert_eq!(moment(&long), None);
        Ok(())
    }

    #[test]
    fn stamps_carry_the_label_they_arrived_with() -> TestResult {
        let zone = "Europe/Zurich";
        let in_zone = stamp_of("2026-11-20T09:00:00.0000000", "Europe/Zurich", zone)?;
        assert_eq!(
            in_zone.timed().map(|t| t.to_string()),
            Some("2026-11-20 09:00".into())
        );
        let lower = stamp_of("2026-11-20T09:00:00", "europe/zurich", zone)?;
        assert!(lower.timed().is_some());
        let utc = stamp_of("2026-11-20T09:00:00", "UTC", zone)?;
        assert_eq!(utc.timed(), None);
        assert_eq!(utc.utc_day(), Some(date("2026-11-20")?));
        let other = stamp_of("2026-11-20T09:00:00", "W. Europe Standard Time", zone)?;
        assert_eq!(other.timed(), None);
        assert_eq!(other.utc_day(), None);
        let on_a_utc_mac = stamp_of("2026-11-20T09:00:00", "UTC", "Etc/UTC")?;
        assert!(on_a_utc_mac.timed().is_some());
        assert!(on_a_utc_mac.utc_day().is_some());
        let same = stamp_of("2026-11-20T09:00:00", "Etc/UTC", "UTC")?;
        assert!(same.timed().is_some());
        for broken in [
            r#"{"dateTime": "2026-11-20T09:00:00"}"#,
            r#"{"timeZone": "UTC"}"#,
            r#"{"dateTime": 5, "timeZone": "UTC"}"#,
            r#"{"dateTime": "2026-11-20T09:00:00", "timeZone": 5}"#,
            r#"{"dateTime": "garbage", "timeZone": "UTC"}"#,
            r#""2026-11-20T09:00:00""#,
            "null",
            "[]",
        ] {
            let value = j(broken)?;
            assert_eq!(stamp(Some(&value), zone), None, "{broken}");
        }
        assert_eq!(stamp(None, zone), None);
        Ok(())
    }

    #[test]
    fn midnight_stamps_are_dates_whatever_their_label() -> TestResult {
        for label in [
            "Europe/Zurich",
            "UTC",
            "Pacific Standard Time",
            "Mars/Olympus",
        ] {
            let stamp = stamp_of("2026-11-20T00:00:00.0000000", label, "Europe/Zurich")?;
            assert_eq!(stamp.day(), Some(date("2026-11-20")?), "{label}");
        }
        Ok(())
    }

    #[test]
    fn a_shifted_stamp_in_the_mac_zone_cannot_be_resolved_alone() -> TestResult {
        for text in [
            "2026-11-20T01:00:00",
            "2026-11-19T16:00:00",
            "2026-11-20T13:00:00",
        ] {
            let stamp = stamp_of(text, "Europe/Zurich", "Europe/Zurich")?;
            assert_eq!(stamp.day(), None, "{text}");
            assert_eq!(stamp.utc_day(), None, "{text}");
        }
        Ok(())
    }

    fn utc_view_of_local_midnight(
        offset_minutes: i64,
    ) -> Result<Stamp, Box<dyn std::error::Error>> {
        let midnight = DateTime::midnight(date("2026-11-20")?);
        let utc = midnight.add_minutes(-offset_minutes).ok_or("utc")?;
        stamp_of(&local_stamp(utc), "UTC", "Europe/Zurich")
    }

    #[test]
    fn the_utc_view_of_an_all_day_event_gives_the_date_for_every_real_zone() -> TestResult {
        let expected = date("2026-11-20")?;
        let zones: [(&str, i64); 36] = [
            ("UTC", 0),
            ("Atlantic/Azores", -60),
            ("America/Noronha", -120),
            ("America/Sao_Paulo", -180),
            ("America/St_Johns", -210),
            ("America/Halifax", -240),
            ("America/New_York", -300),
            ("America/Chicago", -360),
            ("America/Denver", -420),
            ("America/Los_Angeles", -480),
            ("America/Anchorage", -540),
            ("Pacific/Marquesas", -570),
            ("Pacific/Honolulu", -600),
            ("Europe/Zurich", 60),
            ("Europe/Athens", 120),
            ("Europe/Moscow", 180),
            ("Asia/Tehran", 210),
            ("Asia/Dubai", 240),
            ("Asia/Kabul", 270),
            ("Asia/Karachi", 300),
            ("Asia/Kolkata", 330),
            ("Asia/Kathmandu", 345),
            ("Asia/Dhaka", 360),
            ("Asia/Yangon", 390),
            ("Asia/Bangkok", 420),
            ("Asia/Shanghai", 480),
            ("Australia/Eucla", 525),
            ("Asia/Tokyo", 540),
            ("Australia/Adelaide", 570),
            ("Australia/Brisbane", 600),
            ("Australia/Lord_Howe", 630),
            ("Australia/Sydney", 660),
            ("Pacific/Auckland", 720),
            ("Pacific/Chatham", 765),
            ("Pacific/Auckland", 780),
            ("Pacific/Tongatapu", 780),
        ];
        for (name, offset) in zones {
            let stamp = utc_view_of_local_midnight(offset)?;
            assert_eq!(stamp.utc_day(), Some(expected), "{name} {offset}");
        }
        Ok(())
    }

    #[test]
    fn the_three_offsets_that_share_a_time_of_day_with_another_zone_are_pinned() -> TestResult {
        let expected = date("2026-11-20")?;
        let wrong = date("2026-11-21")?;
        assert_eq!(utc_view_of_local_midnight(-660)?.utc_day(), Some(wrong));
        assert_eq!(utc_view_of_local_midnight(-720)?.utc_day(), Some(wrong));
        let kiritimati = utc_view_of_local_midnight(840)?;
        assert_eq!(kiritimati.utc_day(), Some(date("2026-11-19")?));
        assert_ne!(kiritimati.utc_day(), Some(expected));
        Ok(())
    }

    #[test]
    fn the_boundary_hour_belongs_to_the_same_day() -> TestResult {
        let same = stamp_of("2026-11-20T10:00:00", "UTC", "Europe/Zurich")?;
        assert_eq!(same.utc_day(), Some(date("2026-11-20")?));
        let next = stamp_of("2026-11-20T10:01:00", "UTC", "Europe/Zurich")?;
        assert_eq!(next.utc_day(), Some(date("2026-11-21")?));
        let last = stamp_of("9999-12-31T23:00:00", "UTC", "Europe/Zurich")?;
        assert_eq!(last.utc_day(), None);
        Ok(())
    }

    #[test]
    fn instants_and_local_stamps_are_formatted_for_graph() -> TestResult {
        let at = DateTime::parse("2026-11-20 09:05").ok_or("at")?;
        assert_eq!(local_stamp(at), "2026-11-20T09:05:00");
        assert_eq!(instant(at), "2026-11-20T09:05:00Z");
        Ok(())
    }
}
