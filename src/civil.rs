use std::fmt;

pub(crate) const MIN_YEAR: i64 = 1;
pub(crate) const MAX_YEAR: i64 = 9999;
const MINUTES_PER_DAY: i64 = 1440;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    pub(crate) fn index(self) -> i64 {
        match self {
            Weekday::Monday => 0,
            Weekday::Tuesday => 1,
            Weekday::Wednesday => 2,
            Weekday::Thursday => 3,
            Weekday::Friday => 4,
            Weekday::Saturday => 5,
            Weekday::Sunday => 6,
        }
    }

    pub(crate) fn from_index(index: i64) -> Weekday {
        match index.rem_euclid(7) {
            0 => Weekday::Monday,
            1 => Weekday::Tuesday,
            2 => Weekday::Wednesday,
            3 => Weekday::Thursday,
            4 => Weekday::Friday,
            5 => Weekday::Saturday,
            _ => Weekday::Sunday,
        }
    }

    pub(crate) fn from_code(code: &str) -> Option<Weekday> {
        match code {
            "MO" => Some(Weekday::Monday),
            "TU" => Some(Weekday::Tuesday),
            "WE" => Some(Weekday::Wednesday),
            "TH" => Some(Weekday::Thursday),
            "FR" => Some(Weekday::Friday),
            "SA" => Some(Weekday::Saturday),
            "SU" => Some(Weekday::Sunday),
            _ => None,
        }
    }

    pub(crate) fn short(self) -> &'static str {
        match self {
            Weekday::Monday => "Mon",
            Weekday::Tuesday => "Tue",
            Weekday::Wednesday => "Wed",
            Weekday::Thursday => "Thu",
            Weekday::Friday => "Fri",
            Weekday::Saturday => "Sat",
            Weekday::Sunday => "Sun",
        }
    }
}

pub(crate) fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub(crate) fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        _ => 28,
    }
}

pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Date {
    year: i64,
    month: i64,
    day: i64,
}

impl Date {
    pub(crate) fn new(year: i64, month: i64, day: i64) -> Option<Date> {
        let valid = (MIN_YEAR..=MAX_YEAR).contains(&year)
            && (1..=12).contains(&month)
            && (1..=days_in_month(year, month)).contains(&day);
        valid.then_some(Date { year, month, day })
    }

    pub(crate) fn from_days(days: i64) -> Option<Date> {
        let (year, month, day) = civil_from_days(days);
        Date::new(year, month, day)
    }

    pub(crate) fn parse(text: &str) -> Option<Date> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
            return None;
        }
        Date::new(
            digits(text.get(0..4)?)?,
            digits(text.get(5..7)?)?,
            digits(text.get(8..10)?)?,
        )
    }

    pub(crate) fn year(self) -> i64 {
        self.year
    }

    pub(crate) fn month(self) -> i64 {
        self.month
    }

    pub(crate) fn day(self) -> i64 {
        self.day
    }

    pub(crate) fn days(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    pub(crate) fn add_days(self, count: i64) -> Option<Date> {
        Date::from_days(self.days().checked_add(count)?)
    }

    pub(crate) fn weekday(self) -> Weekday {
        Weekday::from_index(self.days() + 3)
    }

    pub(crate) fn month_index(self) -> i64 {
        self.year * 12 + self.month - 1
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DateTime {
    date: Date,
    minute: i64,
}

impl DateTime {
    pub(crate) fn new(date: Date, minute_of_day: i64) -> Option<DateTime> {
        (0..MINUTES_PER_DAY)
            .contains(&minute_of_day)
            .then_some(DateTime {
                date,
                minute: minute_of_day,
            })
    }

    pub(crate) fn midnight(date: Date) -> DateTime {
        DateTime { date, minute: 0 }
    }

    pub(crate) fn parse(text: &str) -> Option<DateTime> {
        let bytes = text.as_bytes();
        if bytes.len() != 16
            || !matches!(bytes.get(10), Some(b' ' | b'T'))
            || bytes.get(13) != Some(&b':')
        {
            return None;
        }
        let date = Date::parse(text.get(0..10)?)?;
        let hour = digits(text.get(11..13)?)?;
        let minute = digits(text.get(14..16)?)?;
        if hour > 23 || minute > 59 {
            return None;
        }
        DateTime::new(date, hour * 60 + minute)
    }

    pub(crate) fn date(self) -> Date {
        self.date
    }

    pub(crate) fn minute_of_day(self) -> i64 {
        self.minute
    }

    pub(crate) fn total_minutes(self) -> i64 {
        self.date.days() * MINUTES_PER_DAY + self.minute
    }

    pub(crate) fn from_total_minutes(total: i64) -> Option<DateTime> {
        let date = Date::from_days(total.div_euclid(MINUTES_PER_DAY))?;
        DateTime::new(date, total.rem_euclid(MINUTES_PER_DAY))
    }

    pub(crate) fn add_minutes(self, count: i64) -> Option<DateTime> {
        DateTime::from_total_minutes(self.total_minutes().checked_add(count)?)
    }

    pub(crate) fn with_date(self, date: Date) -> DateTime {
        DateTime {
            date,
            minute: self.minute,
        }
    }

    pub(crate) fn clock(self) -> String {
        format!("{:02}:{:02}", self.minute / 60, self.minute % 60)
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.date, self.clock())
    }
}

fn digits(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestResult;

    fn date(year: i64, month: i64, day: i64) -> Result<Date, &'static str> {
        Date::new(year, month, day).ok_or("date")
    }

    #[test]
    fn known_dates_have_the_right_weekday_and_day_number() -> TestResult {
        assert_eq!(date(1970, 1, 1)?.days(), 0);
        assert_eq!(date(1970, 1, 1)?.weekday(), Weekday::Thursday);
        assert_eq!(date(2000, 2, 29)?.weekday(), Weekday::Tuesday);
        assert_eq!(date(2026, 11, 13)?.weekday(), Weekday::Friday);
        assert_eq!(date(2100, 1, 1)?.weekday(), Weekday::Friday);
        assert_eq!(date(2023, 11, 14)?.days(), 19_675);
        assert_eq!(date(1969, 12, 31)?.days(), -1);
        Ok(())
    }

    #[test]
    fn days_and_dates_round_trip_over_the_whole_range() -> TestResult {
        let first = date(MIN_YEAR, 1, 1)?.days();
        let last = date(MAX_YEAR, 12, 31)?.days();
        let mut days = first;
        while days <= last {
            let back = Date::from_days(days).ok_or("range")?;
            assert_eq!(back.days(), days);
            days += 37;
        }
        assert_eq!(Date::from_days(first - 1), None);
        assert_eq!(Date::from_days(last + 1), None);
        assert_eq!(Date::from_days(i64::MAX / 2), None);
        Ok(())
    }

    #[test]
    fn leap_years_and_month_lengths() {
        assert!(is_leap(2000) && is_leap(2024) && !is_leap(1900) && !is_leap(2026));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 12), 31);
        assert_eq!(Date::new(2026, 2, 29), None);
        assert!(Date::new(2024, 2, 29).is_some());
    }

    #[test]
    fn invalid_dates_are_rejected() {
        for (year, month, day) in [
            (0, 1, 1),
            (10_000, 1, 1),
            (2026, 0, 1),
            (2026, 13, 1),
            (2026, 1, 0),
            (2026, 4, 31),
            (-5, 1, 1),
        ] {
            assert_eq!(Date::new(year, month, day), None, "{year}-{month}-{day}");
        }
    }

    #[test]
    fn adding_days_crosses_months_years_and_range_edges() -> TestResult {
        assert_eq!(date(2026, 12, 31)?.add_days(1), Date::new(2027, 1, 1));
        assert_eq!(date(2024, 2, 28)?.add_days(1), Date::new(2024, 2, 29));
        assert_eq!(date(2026, 3, 1)?.add_days(-1), Date::new(2026, 2, 28));
        assert_eq!(date(MAX_YEAR, 12, 31)?.add_days(1), None);
        assert_eq!(date(2026, 1, 1)?.add_days(i64::MAX), None);
        Ok(())
    }

    #[test]
    fn dates_parse_strictly() {
        assert_eq!(Date::parse("2026-11-13"), Date::new(2026, 11, 13));
        for bad in [
            "",
            "2026-11-1",
            "2026/11/13",
            "2026-11-13 ",
            "+026-11-13",
            "2026-1-13x",
            "2026-02-30",
            "20261113",
            "2026-11-13T",
            "abcd-ef-gh",
            "２０２６-11-13",
            "2026-11-+3",
        ] {
            assert_eq!(Date::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn date_times_parse_strictly_and_print_back() -> TestResult {
        let parsed = DateTime::parse("2026-11-13 09:05").ok_or("parse")?;
        assert_eq!(parsed.to_string(), "2026-11-13 09:05");
        assert_eq!(DateTime::parse("2026-11-13T09:05"), Some(parsed));
        assert_eq!(parsed.clock(), "09:05");
        for bad in [
            "2026-11-13",
            "2026-11-13 24:00",
            "2026-11-13 09:60",
            "2026-11-13  9:05",
            "2026-11-13 09:5",
            "2026-11-13 09-05",
            "2026-11-13_09:05",
            "2026-02-30 09:05",
            "2026-11-13 09:05:00",
            "",
        ] {
            assert_eq!(DateTime::parse(bad), None, "{bad:?}");
        }
        Ok(())
    }

    #[test]
    fn minutes_add_across_midnight_and_order_chronologically() -> TestResult {
        let late = DateTime::parse("2026-12-31 23:30").ok_or("parse")?;
        assert_eq!(
            late.add_minutes(45).map(|value| value.to_string()),
            Some("2027-01-01 00:15".to_string())
        );
        assert_eq!(
            late.add_minutes(-23 * 60 - 31)
                .map(|value| value.to_string()),
            Some("2026-12-30 23:59".to_string())
        );
        let early = DateTime::parse("2026-12-31 08:00").ok_or("parse")?;
        assert!(early < late);
        assert_eq!(late.total_minutes() - early.total_minutes(), 930);
        assert_eq!(
            DateTime::from_total_minutes(late.total_minutes()),
            Some(late)
        );
        assert_eq!(late.add_minutes(i64::MAX), None);
        assert_eq!(
            late.with_date(date(2027, 5, 5)?).to_string(),
            "2027-05-05 23:30"
        );
        assert_eq!(DateTime::midnight(date(2027, 5, 5)?).clock(), "00:00");
        Ok(())
    }

    #[test]
    fn weekday_codes_and_indexes() {
        for index in -14..14 {
            let day = Weekday::from_index(index);
            assert_eq!(day.index(), index.rem_euclid(7));
        }
        assert_eq!(Weekday::from_code("TU"), Some(Weekday::Tuesday));
        assert_eq!(Weekday::from_code("tu"), None);
        assert_eq!(Weekday::from_code(""), None);
        assert_eq!(Weekday::Sunday.short(), "Sun");
    }

    #[test]
    fn month_index_is_consecutive() -> TestResult {
        assert_eq!(
            date(2027, 1, 15)?.month_index() - date(2026, 12, 1)?.month_index(),
            1
        );
        Ok(())
    }
}
