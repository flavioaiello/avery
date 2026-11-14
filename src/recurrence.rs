use std::collections::BTreeSet;

use crate::civil::{days_in_month, Date, DateTime, Weekday};

const MAX_RULE_CHARS: usize = 400;
const MAX_LIST_ITEMS: usize = 62;
const MAX_INTERVAL: i64 = 1000;
const MAX_COUNT: i64 = 100_000;
const MAX_PERIODS: i64 = 200_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ByDay {
    ordinal: Option<i64>,
    day: Weekday,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Until {
    Date(Date),
    Local(DateTime),
    Utc(DateTime),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    freq: Freq,
    interval: i64,
    count: Option<i64>,
    until: Option<Until>,
    by_day: Vec<ByDay>,
    by_month_day: Vec<i64>,
    by_month: Vec<i64>,
    by_set_pos: Vec<i64>,
    week_start: Weekday,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RuleError {
    Malformed,
    Unsupported(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Exceeded;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Expansion {
    pub(crate) dates: Vec<Date>,
    pub(crate) truncated: bool,
}

impl Rule {
    pub(crate) fn label(&self) -> String {
        let unit = match self.freq {
            Freq::Daily => "day",
            Freq::Weekly => "week",
            Freq::Monthly => "month",
            Freq::Yearly => "year",
        };
        if self.interval == 1 {
            format!("every {unit}")
        } else {
            format!("every {} {unit}s", self.interval)
        }
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), RuleError> {
    if slot.is_some() {
        return Err(RuleError::Malformed);
    }
    *slot = Some(value);
    Ok(())
}

fn number(text: &str) -> Result<i64, RuleError> {
    let unsigned = text.strip_prefix(['+', '-']).map_or(text, |rest| rest);
    if unsigned.is_empty() || unsigned.len() > 9 || !unsigned.bytes().all(|b| b.is_ascii_digit()) {
        return Err(RuleError::Malformed);
    }
    text.strip_prefix('+')
        .unwrap_or(text)
        .parse()
        .map_err(|_| RuleError::Malformed)
}

fn number_list(value: &str, high: i64) -> Result<Vec<i64>, RuleError> {
    let mut found = Vec::new();
    for piece in value.split(',') {
        let item = number(piece)?;
        if item == 0 || item.abs() > high {
            return Err(RuleError::Malformed);
        }
        if !found.contains(&item) {
            found.push(item);
        }
        if found.len() > MAX_LIST_ITEMS {
            return Err(RuleError::Malformed);
        }
    }
    Ok(found)
}

fn month_list(value: &str) -> Result<Vec<i64>, RuleError> {
    let mut found = Vec::new();
    for piece in value.split(',') {
        let item = number(piece)?;
        if !(1..=12).contains(&item) {
            return Err(RuleError::Malformed);
        }
        if !found.contains(&item) {
            found.push(item);
        }
    }
    Ok(found)
}

fn day_list(value: &str) -> Result<Vec<ByDay>, RuleError> {
    let mut found: Vec<ByDay> = Vec::new();
    for piece in value.split(',') {
        if !piece.is_ascii() || piece.len() < 2 {
            return Err(RuleError::Malformed);
        }
        let (ordinal, code) = piece.split_at(piece.len() - 2);
        let day = Weekday::from_code(code).ok_or(RuleError::Malformed)?;
        let ordinal = if ordinal.is_empty() {
            None
        } else {
            let value = number(ordinal)?;
            if value == 0 || value.abs() > 53 {
                return Err(RuleError::Malformed);
            }
            Some(value)
        };
        let entry = ByDay { ordinal, day };
        if !found.contains(&entry) {
            found.push(entry);
        }
        if found.len() > MAX_LIST_ITEMS {
            return Err(RuleError::Malformed);
        }
    }
    Ok(found)
}

fn compact_date(text: &str) -> Result<Date, RuleError> {
    let field = |from: usize, to: usize| -> Result<i64, RuleError> {
        let part = text.get(from..to).ok_or(RuleError::Malformed)?;
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(RuleError::Malformed);
        }
        part.parse().map_err(|_| RuleError::Malformed)
    };
    Date::new(field(0, 4)?, field(4, 6)?, field(6, 8)?).ok_or(RuleError::Malformed)
}

fn until(value: &str) -> Result<Until, RuleError> {
    if !value.is_ascii() {
        return Err(RuleError::Malformed);
    }
    let compact_time = |text: &str| -> Result<DateTime, RuleError> {
        let date = compact_date(text.get(0..8).ok_or(RuleError::Malformed)?)?;
        if text.get(8..9) != Some("T") {
            return Err(RuleError::Malformed);
        }
        let digits = text.get(9..15).ok_or(RuleError::Malformed)?;
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(RuleError::Malformed);
        }
        let part = |from: usize, to: usize| -> Result<i64, RuleError> {
            digits
                .get(from..to)
                .and_then(|piece| piece.parse().ok())
                .ok_or(RuleError::Malformed)
        };
        let (hour, minute) = (part(0, 2)?, part(2, 4)?);
        if hour > 23 || minute > 59 || part(4, 6)? > 60 {
            return Err(RuleError::Malformed);
        }
        DateTime::new(date, hour * 60 + minute).ok_or(RuleError::Malformed)
    };
    match value.len() {
        8 => Ok(Until::Date(compact_date(value)?)),
        15 => Ok(Until::Local(compact_time(value)?)),
        16 if value.ends_with('Z') => Ok(Until::Utc(compact_time(&value[..15])?)),
        _ => Err(RuleError::Malformed),
    }
}

pub(crate) fn parse(text: &str) -> Result<Rule, RuleError> {
    let upper = text.trim().to_ascii_uppercase();
    let body = upper.strip_prefix("RRULE:").unwrap_or(&upper);
    if body.is_empty() || body.chars().count() > MAX_RULE_CHARS || !body.is_ascii() {
        return Err(RuleError::Malformed);
    }
    let mut freq = None;
    let mut interval = None;
    let mut count = None;
    let mut ends = None;
    let mut by_day = None;
    let mut by_month_day = None;
    let mut by_month = None;
    let mut by_set_pos = None;
    let mut week_start = None;
    for part in body.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, value) = part.split_once('=').ok_or(RuleError::Malformed)?;
        match name {
            "FREQ" => set_once(
                &mut freq,
                match value {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    "HOURLY" | "MINUTELY" | "SECONDLY" => {
                        return Err(RuleError::Unsupported("FREQ below DAILY"))
                    }
                    _ => return Err(RuleError::Malformed),
                },
            )?,
            "INTERVAL" => {
                let value = number(value)?;
                if value < 1 {
                    return Err(RuleError::Malformed);
                }
                if value > MAX_INTERVAL {
                    return Err(RuleError::Unsupported("INTERVAL"));
                }
                set_once(&mut interval, value)?;
            }
            "COUNT" => {
                let value = number(value)?;
                if value < 1 {
                    return Err(RuleError::Malformed);
                }
                if value > MAX_COUNT {
                    return Err(RuleError::Unsupported("COUNT"));
                }
                set_once(&mut count, value)?;
            }
            "UNTIL" => set_once(&mut ends, until(value)?)?,
            "BYDAY" => set_once(&mut by_day, day_list(value)?)?,
            "BYMONTHDAY" => set_once(&mut by_month_day, number_list(value, 31)?)?,
            "BYMONTH" => set_once(&mut by_month, month_list(value)?)?,
            "BYSETPOS" => set_once(&mut by_set_pos, number_list(value, 366)?)?,
            "WKST" => set_once(
                &mut week_start,
                Weekday::from_code(value).ok_or(RuleError::Malformed)?,
            )?,
            "BYSECOND" | "BYMINUTE" | "BYHOUR" => return Err(RuleError::Unsupported("BYHOUR")),
            "BYYEARDAY" => return Err(RuleError::Unsupported("BYYEARDAY")),
            "BYWEEKNO" => return Err(RuleError::Unsupported("BYWEEKNO")),
            _ => return Err(RuleError::Unsupported("unknown part")),
        }
    }
    let rule = Rule {
        freq: freq.ok_or(RuleError::Malformed)?,
        interval: interval.unwrap_or(1),
        count,
        until: ends,
        by_day: by_day.unwrap_or_default(),
        by_month_day: by_month_day.unwrap_or_default(),
        by_month: by_month.unwrap_or_default(),
        by_set_pos: by_set_pos.unwrap_or_default(),
        week_start: week_start.unwrap_or(Weekday::Monday),
    };
    if rule.count.is_some() && rule.until.is_some() {
        return Err(RuleError::Malformed);
    }
    check_combination(&rule)?;
    Ok(rule)
}

fn check_combination(rule: &Rule) -> Result<(), RuleError> {
    let ordinals = rule.by_day.iter().any(|entry| entry.ordinal.is_some());
    match rule.freq {
        Freq::Daily => {
            if ordinals {
                return Err(RuleError::Unsupported("BYDAY with a number"));
            }
        }
        Freq::Weekly => {
            if ordinals {
                return Err(RuleError::Unsupported("BYDAY with a number"));
            }
            if !rule.by_month_day.is_empty() {
                return Err(RuleError::Unsupported("BYMONTHDAY in a weekly rule"));
            }
        }
        Freq::Monthly => {
            if ordinals && !rule.by_month_day.is_empty() {
                return Err(RuleError::Unsupported("BYDAY with a number and BYMONTHDAY"));
            }
        }
        Freq::Yearly => {
            if ordinals && (rule.by_month.is_empty() || !rule.by_month_day.is_empty()) {
                return Err(RuleError::Unsupported("BYDAY with a number in a year"));
            }
        }
    }
    Ok(())
}

fn week_start_days(date: Date, week_start: Weekday) -> i64 {
    date.days() - (date.weekday().index() - week_start.index()).rem_euclid(7)
}

fn weekday_matches(entries: &[ByDay], date: Date) -> bool {
    entries.iter().any(|entry| entry.day == date.weekday())
}

fn month_day_matches(entries: &[i64], date: Date) -> bool {
    let last = days_in_month(date.year(), date.month());
    entries.iter().any(|entry| {
        if *entry > 0 {
            *entry == date.day()
        } else {
            last + entry + 1 == date.day()
        }
    })
}

fn month_allows(rule: &Rule, date: Date) -> bool {
    rule.by_month.is_empty() || rule.by_month.contains(&date.month())
}

fn day_passes(rule: &Rule, date: Date) -> bool {
    month_allows(rule, date)
        && (rule.by_month_day.is_empty() || month_day_matches(&rule.by_month_day, date))
        && (rule.by_day.is_empty() || weekday_matches(&rule.by_day, date))
}

fn month_candidates(rule: &Rule, origin: Date, year: i64, month: i64) -> Vec<Date> {
    let last = days_in_month(year, month);
    let mut days: BTreeSet<i64> = BTreeSet::new();
    if !rule.by_month_day.is_empty() {
        for entry in &rule.by_month_day {
            let day = if *entry > 0 { *entry } else { last + entry + 1 };
            if (1..=last).contains(&day) {
                days.insert(day);
            }
        }
        if !rule.by_day.is_empty() {
            days.retain(|day| {
                Date::new(year, month, *day).is_some_and(|date| weekday_matches(&rule.by_day, date))
            });
        }
    } else if !rule.by_day.is_empty() {
        let first_weekday = Date::new(year, month, 1).map_or(0, |date| date.weekday().index());
        for entry in &rule.by_day {
            let first = 1 + (entry.day.index() - first_weekday).rem_euclid(7);
            let matching: Vec<i64> = (first..=last).step_by(7).collect();
            match entry.ordinal {
                None => days.extend(matching),
                Some(ordinal) if ordinal > 0 => {
                    if let Some(day) = usize::try_from(ordinal - 1)
                        .ok()
                        .and_then(|index| matching.get(index))
                    {
                        days.insert(*day);
                    }
                }
                Some(ordinal) => {
                    if let Some(day) = usize::try_from(-ordinal)
                        .ok()
                        .and_then(|back| matching.len().checked_sub(back))
                        .and_then(|index| matching.get(index))
                    {
                        days.insert(*day);
                    }
                }
            }
        }
    } else if origin.day() <= last {
        days.insert(origin.day());
    }
    days.into_iter()
        .filter_map(|day| Date::new(year, month, day))
        .collect()
}

fn period_start(rule: &Rule, origin: Date, index: i64) -> Option<Date> {
    let step = rule.interval.checked_mul(index)?;
    match rule.freq {
        Freq::Daily => origin.add_days(step),
        Freq::Weekly => Date::from_days(
            week_start_days(origin, rule.week_start).checked_add(step.checked_mul(7)?)?,
        ),
        Freq::Monthly => {
            let position = origin.month_index().checked_add(step)?;
            Date::new(position.div_euclid(12), position.rem_euclid(12) + 1, 1)
        }
        Freq::Yearly => Date::new(origin.year().checked_add(step)?, 1, 1),
    }
}

fn pick(sorted: &[Date], positions: &[i64]) -> Vec<Date> {
    let length = i64::try_from(sorted.len()).unwrap_or(i64::MAX);
    let mut chosen: Vec<Date> = Vec::new();
    for position in positions {
        let index = if *position > 0 {
            position - 1
        } else {
            length + position
        };
        if let Some(date) = usize::try_from(index).ok().and_then(|i| sorted.get(i)) {
            chosen.push(*date);
        }
    }
    chosen.sort();
    chosen.dedup();
    chosen
}

fn period_candidates(rule: &Rule, origin: Date, index: i64) -> Option<Vec<Date>> {
    let begin = period_start(rule, origin, index)?;
    let mut days: Vec<Date> = match rule.freq {
        Freq::Daily => {
            if day_passes(rule, begin) {
                vec![begin]
            } else {
                Vec::new()
            }
        }
        Freq::Weekly => (0..7)
            .filter_map(|offset| begin.add_days(offset))
            .filter(|date| {
                month_allows(rule, *date)
                    && if rule.by_day.is_empty() {
                        date.weekday() == origin.weekday()
                    } else {
                        weekday_matches(&rule.by_day, *date)
                    }
            })
            .collect(),
        Freq::Monthly => {
            if rule.by_month.is_empty() || rule.by_month.contains(&begin.month()) {
                month_candidates(rule, origin, begin.year(), begin.month())
            } else {
                Vec::new()
            }
        }
        Freq::Yearly => {
            let months: Vec<i64> = if !rule.by_month.is_empty() {
                rule.by_month.clone()
            } else if rule.by_month_day.is_empty() && rule.by_day.is_empty() {
                vec![origin.month()]
            } else {
                (1..=12).collect()
            };
            months
                .into_iter()
                .flat_map(|month| month_candidates(rule, origin, begin.year(), month))
                .collect()
        }
    };
    days.sort();
    days.dedup();
    if !rule.by_set_pos.is_empty() {
        days = pick(&days, &rule.by_set_pos);
    }
    Some(days)
}

fn first_period(rule: &Rule, origin: Date, from: Date) -> i64 {
    let elapsed = match rule.freq {
        Freq::Daily => from.days() - origin.days(),
        Freq::Weekly => {
            (week_start_days(from, rule.week_start) - week_start_days(origin, rule.week_start)) / 7
        }
        Freq::Monthly => from.month_index() - origin.month_index(),
        Freq::Yearly => from.year() - origin.year(),
    };
    (elapsed.div_euclid(rule.interval) - 1).max(0)
}

const DST_SLACK_MINUTES: i64 = 60;

fn after_until(rule: &Rule, occurrence: DateTime, utc_offset_minutes: i64) -> bool {
    match rule.until {
        None => false,
        Some(Until::Date(last)) => occurrence.date() > last,
        Some(Until::Local(last)) => occurrence > last,
        Some(Until::Utc(last)) => last
            .add_minutes(utc_offset_minutes + DST_SLACK_MINUTES)
            .is_some_and(|local| occurrence > local),
    }
}

pub(crate) fn expand(
    rule: &Rule,
    start: DateTime,
    utc_offset_minutes: i64,
    excluded: &[Date],
    window: (Date, Date),
    max: usize,
    budget: &mut i64,
) -> Result<Expansion, Exceeded> {
    let (from, to) = window;
    let origin = start.date();
    let wanted = |date: Date| from <= date && date < to && !excluded.contains(&date);
    let mut dates: Vec<Date> = Vec::new();
    let mut truncated = false;
    if wanted(origin) {
        if max == 0 {
            return Ok(Expansion {
                dates,
                truncated: true,
            });
        }
        dates.push(origin);
    }
    let mut produced: i64 = 1;
    let mut index = if rule.count.is_some() {
        0
    } else {
        first_period(rule, origin, from)
    };
    let mut periods: i64 = 0;
    'periods: loop {
        periods += 1;
        *budget -= 1;
        if periods > MAX_PERIODS || *budget < 0 {
            return Err(Exceeded);
        }
        let Some(begin) = period_start(rule, origin, index) else {
            break;
        };
        if begin >= to {
            break;
        }
        let Some(candidates) = period_candidates(rule, origin, index) else {
            break;
        };
        for date in candidates {
            if date <= origin {
                continue;
            }
            if after_until(rule, start.with_date(date), utc_offset_minutes) {
                break 'periods;
            }
            produced += 1;
            if rule.count.is_some_and(|count| produced > count) {
                break 'periods;
            }
            if wanted(date) {
                if dates.len() >= max {
                    truncated = true;
                    break 'periods;
                }
                dates.push(date);
            }
        }
        index += 1;
    }
    Ok(Expansion { dates, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestResult;

    fn expand_all(
        rule: &Rule,
        start: DateTime,
        utc_offset_minutes: i64,
        excluded: &[Date],
        window: (Date, Date),
        max: usize,
    ) -> Result<Expansion, Exceeded> {
        let mut budget = 1_000_000;
        expand(
            rule,
            start,
            utc_offset_minutes,
            excluded,
            window,
            max,
            &mut budget,
        )
    }

    fn rule(text: &str) -> Result<Rule, String> {
        parse(text).map_err(|error| format!("{text}: {error:?}"))
    }

    fn day(text: &str) -> Result<Date, &'static str> {
        Date::parse(text).ok_or("date")
    }

    fn run(
        rule: &str,
        start: &str,
        from: &str,
        to: &str,
        excluded: &[&str],
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let parsed = parse(rule).map_err(|error| format!("{rule}: {error:?}"))?;
        let start = DateTime::parse(start).ok_or("start")?;
        let mut skipped = Vec::new();
        for text in excluded {
            skipped.push(day(text)?);
        }
        let result = expand_all(&parsed, start, 0, &skipped, (day(from)?, day(to)?), 1000)
            .map_err(|_| "exceeded")?;
        Ok(result.dates.iter().map(Date::to_string).collect())
    }

    #[test]
    fn daily_with_interval_skips_days() -> TestResult {
        assert_eq!(
            run(
                "FREQ=DAILY;INTERVAL=3",
                "2026-11-01 09:00",
                "2026-11-05",
                "2026-11-20",
                &[]
            )?,
            [
                "2026-11-07",
                "2026-11-10",
                "2026-11-13",
                "2026-11-16",
                "2026-11-19"
            ]
        );
        Ok(())
    }

    #[test]
    fn the_start_is_always_the_first_occurrence() -> TestResult {
        assert_eq!(
            run(
                "FREQ=WEEKLY;BYDAY=TU",
                "2026-11-02 09:00",
                "2026-11-01",
                "2026-11-12",
                &[]
            )?,
            ["2026-11-02", "2026-11-03", "2026-11-10"]
        );
        Ok(())
    }

    #[test]
    fn weekly_on_several_days_with_interval_and_week_start() -> TestResult {
        assert_eq!(
            run(
                "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE,FR",
                "2026-11-02 18:30",
                "2026-11-01",
                "2026-12-06",
                &[]
            )?,
            [
                "2026-11-02",
                "2026-11-04",
                "2026-11-06",
                "2026-11-16",
                "2026-11-18",
                "2026-11-20",
                "2026-11-30",
                "2026-12-02",
                "2026-12-04"
            ]
        );
        assert_eq!(
            run(
                "FREQ=WEEKLY;INTERVAL=2;BYDAY=SU,MO;WKST=SU",
                "2026-11-01 10:00",
                "2026-11-01",
                "2026-11-30",
                &[]
            )?,
            [
                "2026-11-01",
                "2026-11-02",
                "2026-11-15",
                "2026-11-16",
                "2026-11-29"
            ]
        );
        Ok(())
    }

    #[test]
    fn weekly_without_byday_repeats_the_weekday_of_the_start() -> TestResult {
        assert_eq!(
            run(
                "FREQ=WEEKLY",
                "2026-11-04 08:00",
                "2026-11-10",
                "2026-12-01",
                &[]
            )?,
            ["2026-11-11", "2026-11-18", "2026-11-25"]
        );
        Ok(())
    }

    #[test]
    fn monthly_on_the_thirty_first_skips_short_months() -> TestResult {
        assert_eq!(
            run(
                "FREQ=MONTHLY",
                "2026-01-31 12:00",
                "2026-01-01",
                "2026-08-01",
                &[]
            )?,
            ["2026-01-31", "2026-03-31", "2026-05-31", "2026-07-31"]
        );
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYMONTHDAY=-1",
                "2026-01-31 12:00",
                "2026-01-01",
                "2026-05-01",
                &[]
            )?,
            ["2026-01-31", "2026-02-28", "2026-03-31", "2026-04-30"]
        );
        Ok(())
    }

    #[test]
    fn monthly_by_ordinal_weekday() -> TestResult {
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYDAY=2TU",
                "2026-11-10 09:00",
                "2026-11-01",
                "2027-02-01",
                &[]
            )?,
            ["2026-11-10", "2026-12-08", "2027-01-12"]
        );
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYDAY=-1FR",
                "2026-11-27 09:00",
                "2026-11-01",
                "2027-02-01",
                &[]
            )?,
            ["2026-11-27", "2026-12-25", "2027-01-29"]
        );
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYDAY=5MO",
                "2026-11-30 09:00",
                "2026-11-01",
                "2027-04-01",
                &[]
            )?,
            ["2026-11-30", "2027-03-29"]
        );
        Ok(())
    }

    #[test]
    fn set_positions_select_from_the_period() -> TestResult {
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
                "2026-10-30 17:00",
                "2026-10-01",
                "2027-01-01",
                &[]
            )?,
            ["2026-10-30", "2026-11-30", "2026-12-31"]
        );
        assert_eq!(
            run(
                "FREQ=YEARLY;BYMONTH=11;BYDAY=TH;BYSETPOS=4",
                "2026-11-26 09:00",
                "2026-01-01",
                "2030-01-01",
                &[]
            )?,
            ["2026-11-26", "2027-11-25", "2028-11-23", "2029-11-22"]
        );
        Ok(())
    }

    #[test]
    fn yearly_handles_leap_days_and_month_lists() -> TestResult {
        assert_eq!(
            run(
                "FREQ=YEARLY",
                "2024-02-29 08:00",
                "2024-01-01",
                "2033-01-01",
                &[]
            )?,
            ["2024-02-29", "2028-02-29", "2032-02-29"]
        );
        assert_eq!(
            run(
                "FREQ=YEARLY;BYMONTH=3,9;BYMONTHDAY=15",
                "2026-03-15 08:00",
                "2026-01-01",
                "2028-01-01",
                &[]
            )?,
            ["2026-03-15", "2026-09-15", "2027-03-15", "2027-09-15"]
        );
        Ok(())
    }

    #[test]
    fn count_includes_excluded_occurrences() -> TestResult {
        assert_eq!(
            run(
                "FREQ=DAILY;COUNT=5",
                "2026-11-01 09:00",
                "2026-11-01",
                "2026-12-01",
                &[]
            )?,
            [
                "2026-11-01",
                "2026-11-02",
                "2026-11-03",
                "2026-11-04",
                "2026-11-05"
            ]
        );
        assert_eq!(
            run(
                "FREQ=DAILY;COUNT=5",
                "2026-11-01 09:00",
                "2026-11-01",
                "2026-12-01",
                &["2026-11-03"]
            )?,
            ["2026-11-01", "2026-11-02", "2026-11-04", "2026-11-05"]
        );
        assert_eq!(
            run(
                "FREQ=DAILY;COUNT=1",
                "2026-11-01 09:00",
                "2026-11-01",
                "2026-12-01",
                &[]
            )?,
            ["2026-11-01"]
        );
        Ok(())
    }

    #[test]
    fn until_in_its_three_forms() -> TestResult {
        assert_eq!(
            run(
                "FREQ=DAILY;UNTIL=20261103",
                "2026-11-01 09:00",
                "2026-11-01",
                "2026-12-01",
                &[]
            )?,
            ["2026-11-01", "2026-11-02", "2026-11-03"]
        );
        assert_eq!(
            run(
                "FREQ=DAILY;UNTIL=20261103T090000",
                "2026-11-01 09:00",
                "2026-11-01",
                "2026-12-01",
                &[]
            )?,
            ["2026-11-01", "2026-11-02", "2026-11-03"]
        );
        assert_eq!(
            run(
                "FREQ=DAILY;UNTIL=20261103T085900",
                "2026-11-01 09:00",
                "2026-11-01",
                "2026-12-01",
                &[]
            )?,
            ["2026-11-01", "2026-11-02"]
        );
        let rule = rule("FREQ=DAILY;UNTIL=20261103T080000Z")?;
        let start = DateTime::parse("2026-11-01 09:00").ok_or("start")?;
        let window = (day("2026-11-01")?, day("2026-12-01")?);
        let in_winter = expand_all(&rule, start, 60, &[], window, 100).map_err(|_| "exceeded")?;
        assert_eq!(in_winter.dates.len(), 3);
        let without_offset =
            expand_all(&rule, start, 0, &[], window, 100).map_err(|_| "exceeded")?;
        assert_eq!(without_offset.dates.len(), 3);
        let offset_too_low =
            expand_all(&rule, start, -60, &[], window, 100).map_err(|_| "exceeded")?;
        assert_eq!(offset_too_low.dates.len(), 2);
        Ok(())
    }

    #[test]
    fn a_utc_until_at_the_last_occurrence_survives_a_change_of_the_utc_offset() -> TestResult {
        let rule = rule("FREQ=DAILY;UNTIL=20260701T070000Z")?;
        let start = DateTime::parse("2026-03-02 09:00").ok_or("start")?;
        let window = (day("2026-06-29")?, day("2026-07-04")?);
        let summer = expand_all(&rule, start, 60, &[], window, 100).map_err(|_| "exceeded")?;
        let dates: Vec<String> = summer.dates.iter().map(|date| date.to_string()).collect();
        assert_eq!(dates, ["2026-06-29", "2026-06-30", "2026-07-01"]);
        let earlier = self::rule("FREQ=DAILY;UNTIL=20260701T053000Z")?;
        let cut = expand_all(&earlier, start, 60, &[], window, 100).map_err(|_| "exceeded")?;
        assert_eq!(cut.dates.len(), 2);
        let winter_start = DateTime::parse("2026-10-05 09:00").ok_or("start")?;
        let until_in_winter = self::rule("FREQ=DAILY;UNTIL=20261110T080000Z")?;
        let window = (day("2026-11-08")?, day("2026-11-13")?);
        let kept = expand_all(&until_in_winter, winter_start, 120, &[], window, 100)
            .map_err(|_| "exceeded")?;
        let dates: Vec<String> = kept.dates.iter().map(|date| date.to_string()).collect();
        assert_eq!(dates, ["2026-11-08", "2026-11-09", "2026-11-10"]);
        Ok(())
    }

    #[test]
    fn the_window_clips_and_long_running_series_start_late() -> TestResult {
        assert_eq!(
            run(
                "FREQ=DAILY",
                "1999-01-01 09:00",
                "2026-11-13",
                "2026-11-16",
                &[]
            )?,
            ["2026-11-13", "2026-11-14", "2026-11-15"]
        );
        assert_eq!(
            run(
                "FREQ=WEEKLY;BYDAY=SU",
                "2010-01-03 18:00",
                "2026-11-13",
                "2026-11-30",
                &[]
            )?,
            ["2026-11-15", "2026-11-22", "2026-11-29"]
        );
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYMONTHDAY=1",
                "2001-05-01 09:00",
                "2026-11-01",
                "2027-01-01",
                &[]
            )?,
            ["2026-11-01", "2026-12-01"]
        );
        assert!(run(
            "FREQ=DAILY;COUNT=3",
            "2020-01-01 09:00",
            "2026-11-13",
            "2026-12-01",
            &[]
        )?
        .is_empty());
        assert!(run(
            "FREQ=DAILY",
            "2030-01-01 09:00",
            "2026-11-13",
            "2026-12-01",
            &[]
        )?
        .is_empty());
        Ok(())
    }

    #[test]
    fn at_most_max_dates_and_a_flag() -> TestResult {
        let rule = rule("FREQ=DAILY")?;
        let start = DateTime::parse("2026-01-01 09:00").ok_or("start")?;
        let result = expand_all(
            &rule,
            start,
            0,
            &[],
            (day("2026-01-01")?, day("2027-01-01")?),
            10,
        )
        .map_err(|_| "exceeded")?;
        assert_eq!(result.dates.len(), 10);
        assert!(result.truncated);
        let none = expand_all(
            &rule,
            start,
            0,
            &[],
            (day("2026-01-01")?, day("2027-01-01")?),
            0,
        )
        .map_err(|_| "exceeded")?;
        assert!(none.dates.is_empty() && none.truncated);
        let exact = expand_all(
            &rule,
            start,
            0,
            &[],
            (day("2026-01-01")?, day("2026-01-11")?),
            10,
        )
        .map_err(|_| "exceeded")?;
        assert_eq!(exact.dates.len(), 10);
        assert!(!exact.truncated);
        Ok(())
    }

    #[test]
    fn rules_that_never_match_produce_only_the_first_occurrence() -> TestResult {
        let counted = rule("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30;COUNT=5")?;
        let start = DateTime::parse("2026-02-10 09:00").ok_or("start")?;
        let outcome = expand_all(
            &counted,
            start,
            0,
            &[],
            (day("2026-01-01")?, day("2400-01-01")?),
            10,
        );
        assert_eq!(outcome.map_err(|_| "exceeded")?.dates, [day("2026-02-10")?]);
        let open = rule("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30")?;
        let outcome = expand_all(
            &open,
            start,
            0,
            &[],
            (day("2026-01-01")?, day("2027-01-01")?),
            10,
        )
        .map_err(|_| "exceeded")?;
        assert_eq!(outcome.dates, [day("2026-02-10")?]);
        Ok(())
    }

    #[test]
    fn a_shared_budget_stops_the_work_of_many_expensive_series() -> TestResult {
        let counted = rule("FREQ=DAILY;COUNT=50000")?;
        let start = DateTime::parse("2000-01-01 09:00").ok_or("start")?;
        let window = (day("2026-11-01")?, day("2026-11-08")?);
        let mut budget = 25_000;
        let first = expand(&counted, start, 0, &[], window, 100, &mut budget);
        assert!(first.is_ok());
        assert!(budget < 25_000 - 9000 && budget > 0, "{budget}");
        let second = expand(&counted, start, 0, &[], window, 100, &mut budget);
        assert!(second.is_ok());
        let third = expand(&counted, start, 0, &[], window, 100, &mut budget);
        assert_eq!(third, Err(Exceeded));
        let mut empty = 0;
        assert_eq!(
            expand(&counted, start, 0, &[], window, 100, &mut empty),
            Err(Exceeded)
        );
        let open = rule("FREQ=DAILY")?;
        let mut small = 50;
        assert!(expand(&open, start, 0, &[], window, 100, &mut small).is_ok());
        let mut tiny = 3;
        assert_eq!(
            expand(&open, start, 0, &[], window, 100, &mut tiny),
            Err(Exceeded)
        );
        Ok(())
    }

    #[test]
    fn the_end_of_the_supported_years_ends_the_series() -> TestResult {
        assert_eq!(
            run(
                "FREQ=YEARLY",
                "9998-06-01 09:00",
                "9998-01-01",
                "9999-12-31",
                &[]
            )?,
            ["9998-06-01", "9999-06-01"]
        );
        Ok(())
    }

    #[test]
    fn rules_parse_leniently_in_case_and_prefix() -> TestResult {
        let plain = rule("FREQ=WEEKLY;BYDAY=MO,TH")?;
        assert_eq!(rule("rrule:freq=weekly;byday=mo,th;")?, plain);
        assert_eq!(rule(" RRULE:FREQ=WEEKLY;BYDAY=TH,MO ")?.by_day.len(), 2);
        assert_eq!(plain.label(), "every week");
        assert_eq!(rule("FREQ=MONTHLY;INTERVAL=3")?.label(), "every 3 months");
        assert_eq!(rule("FREQ=DAILY")?.label(), "every day");
        assert_eq!(rule("FREQ=YEARLY")?.label(), "every year");
        Ok(())
    }

    #[test]
    fn unsupported_rules_are_named_not_guessed() {
        for (text, part) in [
            ("FREQ=HOURLY", "FREQ below DAILY"),
            ("FREQ=DAILY;BYHOUR=9", "BYHOUR"),
            ("FREQ=YEARLY;BYWEEKNO=20", "BYWEEKNO"),
            ("FREQ=YEARLY;BYYEARDAY=100", "BYYEARDAY"),
            ("FREQ=WEEKLY;BYMONTHDAY=3", "BYMONTHDAY in a weekly rule"),
            ("FREQ=WEEKLY;BYDAY=2MO", "BYDAY with a number"),
            ("FREQ=DAILY;BYDAY=-1MO", "BYDAY with a number"),
            (
                "FREQ=MONTHLY;BYDAY=2MO;BYMONTHDAY=3",
                "BYDAY with a number and BYMONTHDAY",
            ),
            ("FREQ=YEARLY;BYDAY=2MO", "BYDAY with a number in a year"),
            ("FREQ=DAILY;X-FOO=1", "unknown part"),
            ("FREQ=DAILY;INTERVAL=5000", "INTERVAL"),
            ("FREQ=DAILY;COUNT=999999", "COUNT"),
        ] {
            assert_eq!(parse(text), Err(RuleError::Unsupported(part)), "{text}");
        }
    }

    #[test]
    fn malformed_rules_are_rejected() {
        let long = format!("FREQ=DAILY;BYMONTHDAY={}", vec!["1"; 500].join(","));
        let many: Vec<String> = (1..=53).chain(-10..=-1).map(|n| format!("{n}MO")).collect();
        let too_many = format!("FREQ=MONTHLY;BYDAY={}", many.join(","));
        for text in [
            "",
            "RRULE:",
            "FREQ",
            "FREQ=",
            "FREQ=SOMETIMES",
            "INTERVAL=2",
            "FREQ=DAILY;FREQ=DAILY",
            "FREQ=DAILY;INTERVAL=0",
            "FREQ=DAILY;INTERVAL=-1",
            "FREQ=DAILY;INTERVAL=x",
            "FREQ=DAILY;COUNT=0",
            "FREQ=DAILY;COUNT=3;UNTIL=20261231",
            "FREQ=DAILY;UNTIL=2026-12-31",
            "FREQ=DAILY;UNTIL=20261301",
            "FREQ=DAILY;UNTIL=20261231T250000",
            "FREQ=DAILY;UNTIL=20261231T0900",
            "FREQ=DAILY;UNTIL=20261231X090000",
            "FREQ=WEEKLY;BYDAY=XX",
            "FREQ=WEEKLY;BYDAY=",
            "FREQ=WEEKLY;BYDAY=M",
            "FREQ=MONTHLY;BYDAY=99MO",
            "FREQ=MONTHLY;BYDAY=0MO",
            "FREQ=MONTHLY;BYMONTHDAY=0",
            "FREQ=MONTHLY;BYMONTHDAY=32",
            "FREQ=MONTHLY;BYMONTHDAY=-32",
            "FREQ=MONTHLY;BYMONTHDAY=a",
            "FREQ=YEARLY;BYMONTH=13",
            "FREQ=YEARLY;BYMONTH=0",
            "FREQ=MONTHLY;BYSETPOS=0",
            "FREQ=MONTHLY;BYSETPOS=400",
            "FREQ=WEEKLY;WKST=XX",
            "FREQ=DAILY;;;FOO",
            "FREQ=DAILY\u{e4}",
            "FREQ=DAILY;INTERVAL=1234567890123",
            long.as_str(),
            too_many.as_str(),
        ] {
            assert_eq!(parse(text), Err(RuleError::Malformed), "{text:?}");
        }
    }

    #[test]
    fn fuzzed_text_never_panics() {
        let alphabet = b"FREQ=DAILYWEKMONTHYBSTUCIVA;,0123456789-+ZT:";
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        for _ in 0..3000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let length = usize::try_from((state >> 33) % 60).unwrap_or(0);
            let mut text = String::new();
            for step in 0..length {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let index =
                    usize::try_from((state >> 33) % u64::try_from(alphabet.len()).unwrap_or(1))
                        .unwrap_or(0);
                if let Some(byte) = alphabet.get(index) {
                    text.push(char::from(*byte));
                }
                if step == 0 {
                    text.insert_str(0, "FREQ=");
                }
            }
            let _ = parse(&text);
        }
    }

    #[test]
    fn oracle_cases_agree_with_the_reference_implementation() -> TestResult {
        let cases = include_str!("../tests/data/recurrence_cases.txt");
        let mut checked = 0usize;
        for line in cases.lines().filter(|line| !line.is_empty()) {
            let fields: Vec<&str> = line.split('|').collect();
            let [rule, start, from, to, excluded, expected] = fields.as_slice() else {
                return Err(format!("bad fixture line: {line}").into());
            };
            let excluded: Vec<&str> = excluded.split(',').filter(|s| !s.is_empty()).collect();
            let got = run(rule, start, from, to, &excluded)?;
            let expected: Vec<&str> = expected.split(',').filter(|s| !s.is_empty()).collect();
            assert_eq!(got, expected, "{line}");
            checked += 1;
        }
        assert!(checked >= 200, "only {checked} cases");
        Ok(())
    }
}
