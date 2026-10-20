//! The `on: schedule` cron dialect, as GitHub Actions reads it.
//!
//! Five fields, evaluated in UTC: minute, hour, day of month, month, day of
//! week. Each field is a comma-separated list of `*`, a value, a range `a-b`,
//! or either of those with a `/step`. Months and weekdays may also be written
//! by their three-letter English names, in any case. That is the POSIX `crontab`
//! syntax GitHub documents, and nothing more:
//!
//! - The `@hourly` / `@daily` / `@weekly` family is refused by name. GitHub
//!   does not accept it, so a workflow that spells it has never run there, and
//!   reading it here would make the same file mean two different things.
//! - A sixth field (seconds, or a year) is refused: one of the cron dialects
//!   that has it would read `0 0 3 * * *` as "03:00 every day" and another as
//!   "midnight on the 3rd".
//! - `N/step` with no range is read the way Gitea's scheduler reads it — the
//!   range `N-<max>` with that step — because a workflow moved from Gitea
//!   carries that spelling, and that is the only meaning any dialect gives it.
//!
//! Day of month and day of week follow the classic rule: when both are
//! restricted (neither starts with `*`), a day matches if *either* does —
//! `0 0 1 * MON` fires on the 1st and on every Monday. When either starts with
//! `*`, both have to match, which with the other one starred is just the other.
//!
//! A schedule that can never fire — `0 0 30 2 *` — is refused rather than
//! accepted: it would parse, be listed as a schedule, and never produce a run,
//! which is the silence `on:` validation exists to prevent.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};

/// How far back [`CronSchedule::latest_tick_at_or_before`] looks for a day the
/// schedule fires on.
///
/// Long enough for the rarest schedule a five-field cron can say: February 29th
/// restricted to one weekday recurs only every 28 years, but nothing that rare
/// is worth a wider search — a schedule whose last tick is further away than
/// this has, for the scheduler's purposes, not fired.
const LOOKBACK_DAYS: i64 = 366 * 8;

/// A parsed `on: schedule` cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    minutes: u64,
    hours: u32,
    days_of_month: u32,
    months: u16,
    days_of_week: u8,
    /// Whether the day-of-month field was written starting with `*`.
    dom_star: bool,
    /// Whether the day-of-week field was written starting with `*`.
    dow_star: bool,
}

struct Field {
    name: &'static str,
    min: u32,
    max: u32,
    names: &'static [&'static str],
}

const MINUTE: Field = Field {
    name: "minute",
    min: 0,
    max: 59,
    names: &[],
};
const HOUR: Field = Field {
    name: "hour",
    min: 0,
    max: 23,
    names: &[],
};
const DAY_OF_MONTH: Field = Field {
    name: "day of month",
    min: 1,
    max: 31,
    names: &[],
};
const MONTH: Field = Field {
    name: "month",
    min: 1,
    max: 12,
    names: &[
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ],
};
const DAY_OF_WEEK: Field = Field {
    name: "day of week",
    min: 0,
    max: 6,
    names: &["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"],
};

impl Field {
    fn value(&self, text: &str) -> Result<u32, String> {
        if let Some(index) = self
            .names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(text))
        {
            return Ok(self.min + index as u32);
        }
        let value: u32 = text
            .parse()
            .ok()
            .filter(|_| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or_else(|| {
                let names = if self.names.is_empty() {
                    String::new()
                } else {
                    format!(" or one of {}", self.names.join(", "))
                };
                format!(
                    "the {} field has `{text}`, which is not a number from {} to {}{names}",
                    self.name, self.min, self.max
                )
            })?;
        if !(self.min..=self.max).contains(&value) {
            return Err(format!(
                "the {} field has {value}, outside {}-{}",
                self.name, self.min, self.max
            ));
        }
        Ok(value)
    }

    /// The set of values one field selects, as a bit mask indexed by value.
    fn parse(&self, text: &str) -> Result<u64, String> {
        let mut mask = 0u64;
        for item in text.split(',') {
            if item.is_empty() {
                return Err(format!(
                    "the {} field `{text}` has an empty list entry",
                    self.name
                ));
            }
            let (range, step) = match item.split_once('/') {
                Some((range, step)) => {
                    let step: u32 =
                        step.parse().ok().filter(|step| *step > 0).ok_or_else(|| {
                            format!(
                                "the {} field `{item}` has the step `{step}`, which is not a \
                                 positive number",
                                self.name
                            )
                        })?;
                    (range, Some(step))
                }
                None => (item, None),
            };
            let (start, end) = if range == "*" {
                (self.min, self.max)
            } else if let Some((start, end)) = range.split_once('-') {
                let (start, end) = (self.value(start)?, self.value(end)?);
                if start > end {
                    return Err(format!(
                        "the {} field `{item}` is a range that runs backwards",
                        self.name
                    ));
                }
                (start, end)
            } else {
                let start = self.value(range)?;
                // `N/step` reads as `N-<max>/step`; a bare value is itself.
                (start, if step.is_some() { self.max } else { start })
            };
            let step = step.unwrap_or(1);
            let mut value = start;
            while value <= end {
                mask |= 1 << value;
                value += step;
            }
        }
        Ok(mask)
    }
}

/// Days in `month` of a leap year — the most a schedule can ever see.
fn longest_month(month: u32) -> u32 {
    match month {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl CronSchedule {
    /// Parse one `- cron:` value, or say what is wrong with it in a sentence
    /// that names the field.
    pub fn parse(expression: &str) -> Result<Self, String> {
        let trimmed = expression.trim();
        if let Some(macro_name) = trimmed.strip_prefix('@') {
            return Err(format!(
                "`@{macro_name}` is a cron shorthand GitHub Actions does not accept; write the \
                 five fields out (`@daily` is `0 0 * * *`)"
            ));
        }
        let fields = trimmed.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(format!(
                "a schedule has five fields — minute, hour, day of month, month, day of week — \
                 and `{trimmed}` has {}",
                fields.len()
            ));
        }
        let schedule = Self {
            minutes: MINUTE.parse(fields[0])?,
            hours: HOUR.parse(fields[1])? as u32,
            days_of_month: DAY_OF_MONTH.parse(fields[2])? as u32,
            months: MONTH.parse(fields[3])? as u16,
            days_of_week: DAY_OF_WEEK.parse(fields[4])? as u8,
            dom_star: fields[2].starts_with('*'),
            dow_star: fields[4].starts_with('*'),
        };
        if !schedule.can_fire() {
            return Err(format!(
                "`{trimmed}` names a day of the month none of its months has, so it never fires"
            ));
        }
        Ok(schedule)
    }

    /// Whether some calendar day matches. Only day of month against month can
    /// rule every day out: a restricted weekday always comes round, and with
    /// both restricted either one is enough.
    fn can_fire(&self) -> bool {
        if !self.dom_star && !self.dow_star {
            return true;
        }
        (1..=12).any(|month| {
            self.months & (1 << month) != 0
                && (1..=longest_month(month)).any(|day| self.days_of_month & (1 << day) != 0)
        })
    }

    fn matches_day(&self, date: NaiveDate) -> bool {
        if self.months & (1 << date.month()) == 0 {
            return false;
        }
        let dom = self.days_of_month & (1 << date.day()) != 0;
        let dow = self.days_of_week & (1 << date.weekday().num_days_from_sunday()) != 0;
        if self.dom_star || self.dow_star {
            dom && dow
        } else {
            dom || dow
        }
    }

    /// The latest moment `<= at` this schedule fires, to the minute.
    ///
    /// `None` when it has not fired within [`LOOKBACK_DAYS`] — which for a
    /// schedule [`Self::parse`] accepted means one rarer than the scheduler
    /// cares about.
    pub fn latest_tick_at_or_before(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let at = at.with_second(0)?.with_nanosecond(0)?;
        let mut date = at.date_naive();
        let mut limit = (at.hour(), at.minute());
        for _ in 0..=LOOKBACK_DAYS {
            if self.matches_day(date) {
                if let Some((hour, minute)) = self.latest_time_at_or_before(limit) {
                    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
                    return Some(Utc.from_utc_datetime(&date.and_time(time)));
                }
            }
            date = date.checked_sub_signed(Duration::days(1))?;
            limit = (23, 59);
        }
        None
    }

    fn latest_time_at_or_before(&self, (hour, minute): (u32, u32)) -> Option<(u32, u32)> {
        for candidate_hour in (0..=hour).rev() {
            if self.hours & (1 << candidate_hour) == 0 {
                continue;
            }
            let top = if candidate_hour == hour { minute } else { 59 };
            if let Some(candidate_minute) = (0..=top)
                .rev()
                .find(|candidate| self.minutes & (1 << candidate) != 0)
            {
                return Some((candidate_hour, candidate_minute));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("test timestamp")
            .with_timezone(&Utc)
    }

    fn latest(expression: &str, now: &str) -> String {
        CronSchedule::parse(expression)
            .unwrap_or_else(|error| panic!("`{expression}` must parse: {error}"))
            .latest_tick_at_or_before(at(now))
            .unwrap_or_else(|| panic!("`{expression}` has a tick before {now}"))
            .to_rfc3339()
    }

    #[test]
    fn the_latest_tick_is_found_to_the_minute() {
        assert_eq!(
            latest("0 3 * * *", "2026-10-20T14:37:59Z"),
            "2026-10-20T03:00:00+00:00"
        );
        assert_eq!(
            latest("0 3 * * *", "2026-10-20T02:59:00Z"),
            "2026-10-19T03:00:00+00:00"
        );
        // A tick exactly at `now` is the latest one.
        assert_eq!(
            latest("*/15 * * * *", "2026-10-20T14:45:00Z"),
            "2026-10-20T14:45:00+00:00"
        );
        assert_eq!(
            latest("*/15 * * * *", "2026-10-20T14:44:59Z"),
            "2026-10-20T14:30:00+00:00"
        );
        assert_eq!(
            latest("30 9-17/4 * * *", "2026-10-20T16:10:00Z"),
            "2026-10-20T13:30:00+00:00"
        );
    }

    #[test]
    fn names_ranges_and_lists_select_what_they_say() {
        // 2026-10-20 is a Tuesday.
        assert_eq!(
            latest("0 12 * * mon,Fri", "2026-10-20T10:00:00Z"),
            "2026-10-19T12:00:00+00:00"
        );
        assert_eq!(
            latest("0 0 1 JAN-MAR *", "2026-10-20T00:00:00Z"),
            "2026-03-01T00:00:00+00:00"
        );
        assert_eq!(
            latest("15,45 * * * *", "2026-10-20T10:44:00Z"),
            "2026-10-20T10:15:00+00:00"
        );
        // Gitea's `N/step`: the range N-<max>.
        assert_eq!(
            latest("5/20 * * * *", "2026-10-20T10:44:00Z"),
            "2026-10-20T10:25:00+00:00"
        );
    }

    #[test]
    fn day_of_month_and_day_of_week_follow_the_classic_rule() {
        // Both restricted: either one is enough — the 1st, or any Monday.
        assert_eq!(
            latest("0 0 1 * MON", "2026-10-20T00:00:00Z"),
            "2026-10-19T00:00:00+00:00"
        );
        assert_eq!(
            latest("0 0 1 * MON", "2026-10-02T00:00:00Z"),
            "2026-10-01T00:00:00+00:00"
        );
        // A starred day of month leaves the weekday as the only condition.
        assert_eq!(
            latest("0 0 * * SUN", "2026-10-20T00:00:00Z"),
            "2026-10-18T00:00:00+00:00"
        );
        // `*/2` starts with `*`, so it is ANDed, not ORed, with the weekday.
        assert_eq!(
            latest("0 0 */2 * MON", "2026-10-20T00:00:00Z"),
            "2026-10-19T00:00:00+00:00"
        );
        assert_eq!(
            latest("0 0 */2 * TUE", "2026-10-20T00:00:00Z"),
            "2026-10-13T00:00:00+00:00"
        );
    }

    #[test]
    fn a_leap_day_schedule_reaches_back_to_the_last_leap_year() {
        assert_eq!(
            latest("0 0 29 2 *", "2026-10-20T00:00:00Z"),
            "2024-02-29T00:00:00+00:00"
        );
    }

    #[test]
    fn what_github_does_not_accept_is_refused_by_name() {
        for (expression, named) in [
            ("@daily", "@daily"),
            ("@hourly", "five fields"),
            ("0 0 * *", "has 4"),
            ("0 0 3 * * *", "has 6"),
            ("60 * * * *", "outside 0-59"),
            ("* 24 * * *", "outside 0-23"),
            ("* * 0 * *", "outside 1-31"),
            ("* * * 13 *", "outside 1-12"),
            ("* * * * 7", "outside 0-6"),
            ("* * * * MONDAY", "MON"),
            ("*/0 * * * *", "positive number"),
            ("10-5 * * * *", "runs backwards"),
            ("1,,2 * * * *", "empty list entry"),
            ("? * * * *", "not a number"),
            ("0 0 30 2 *", "never fires"),
            ("0 0 31 4,6,9,11 *", "never fires"),
        ] {
            let error = CronSchedule::parse(expression)
                .expect_err(&format!("`{expression}` must be refused"));
            assert!(
                error.contains(named),
                "`{expression}`: the refusal must say why ({named}), got: {error}"
            );
        }
    }

    #[test]
    fn a_never_firing_day_rescued_by_a_weekday_is_accepted() {
        // Both restricted, so the weekday alone is enough to fire.
        CronSchedule::parse("0 0 30 2 MON").expect("a Monday always comes round");
    }
}
