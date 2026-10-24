//! Calendar days as a single integer, so the heat map can do arithmetic on them.
//!
//! A day is the number of days since 1970-01-01, which makes comparing, adding
//! and bucketing trivial. The conversions are Howard Hinnant's `days_from_civil`
//! and `civil_from_days`: exact for the whole proleptic Gregorian range we can
//! get out of a system clock, and cheap enough to run per log line.
//!
//! "Today" is the local calendar day. Asking the clock for a UTC day instead
//! leaves the board one day behind for everyone east of Greenwich until eight
//! in the morning.

#[repr(C)]
#[derive(Default)]
struct LocalTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLocalTime(out: *mut LocalTime);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(pub i64);

impl Day {
    pub fn today() -> Self {
        let mut now = LocalTime::default();
        unsafe { GetLocalTime(&mut now) };
        Self::from_date(
            i64::from(now.year),
            i64::from(now.month),
            i64::from(now.day),
        )
    }

    pub fn from_date(year: i64, month: i64, day: i64) -> Self {
        let year = year - i64::from(month <= 2);
        let era = if year >= 0 { year } else { year - 399 } / 400;
        let year_of_era = year - era * 400;
        let shifted = month + if month > 2 { -3 } else { 9 };
        let day_of_year = (153 * shifted + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        Self(era * 146_097 + day_of_era - 719_468)
    }

    pub fn date(self) -> (i64, i64, i64) {
        let shifted = self.0 + 719_468;
        let era = if shifted >= 0 {
            shifted
        } else {
            shifted - 146_096
        } / 146_097;
        let day_of_era = shifted - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_shift = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_shift + 2) / 5 + 1;
        let month = month_shift + if month_shift < 10 { 3 } else { -9 };
        (year + i64::from(month <= 2), month, day)
    }

    /// 0 is Sunday, the order GitHub lays its rows out in.
    pub fn weekday(self) -> i64 {
        (self.0 + 4).rem_euclid(7)
    }

    /// The Sunday that opens the column this day belongs to.
    pub fn week_start(self) -> Self {
        Self(self.0 - self.weekday())
    }

    pub fn plus(self, days: i64) -> Self {
        Self(self.0 + days)
    }

    /// `YYYY-MM-DD`, the form the log stores and the status bar shows.
    pub fn label(self) -> String {
        let (year, month, day) = self.date();
        format!("{year:04}-{month:02}-{day:02}")
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.splitn(3, '-');
        let year = parts.next()?.parse().ok()?;
        let month = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        let candidate = Self::from_date(year, month, day);
        // `from_date` happily rolls 2026-02-31 into March; the log should never
        // hold that, and reading it back is how we notice.
        (candidate.date() == (year, month, day)).then_some(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_days_land_where_the_calendar_says() {
        assert_eq!(Day::from_date(1970, 1, 1).0, 0);
        assert_eq!(Day::from_date(2000, 1, 1).0, 10_957);
        assert_eq!(Day::from_date(2024, 2, 29).0, 19_782);
        assert_eq!(Day(10_957).date(), (2000, 1, 1));
    }

    #[test]
    fn weekdays_line_up_with_the_calendar() {
        // 1970-01-01 was a Thursday, 2000-01-01 a Saturday.
        assert_eq!(Day(0).weekday(), 4);
        assert_eq!(Day::from_date(2000, 1, 1).weekday(), 6);
        assert_eq!(Day::from_date(2026, 10, 18).weekday(), 0);
        assert_eq!(
            Day::from_date(2026, 10, 18).week_start().label(),
            "2026-10-18"
        );
        assert_eq!(
            Day::from_date(2026, 10, 22).week_start().label(),
            "2026-10-18"
        );
    }

    #[test]
    fn every_day_of_a_four_year_cycle_round_trips() {
        let start = Day::from_date(2020, 1, 1);
        for offset in 0..1462 {
            let day = start.plus(offset);
            let (year, month, date) = day.date();
            assert_eq!(
                Day::from_date(year, month, date),
                day,
                "{year}-{month}-{date}"
            );
        }
    }

    #[test]
    fn labels_parse_only_real_dates() {
        assert_eq!(
            Day::parse("2026-10-22").map(Day::label).as_deref(),
            Some("2026-10-22")
        );
        assert_eq!(Day::parse("2026-2-31"), None);
        assert_eq!(Day::parse("2026-13-01"), None);
        assert_eq!(Day::parse("2026-10"), None);
        assert_eq!(Day::parse("2026-10-22x"), None);
        assert_eq!(
            Day::parse("2024-02-29").map(Day::label).as_deref(),
            Some("2024-02-29")
        );
    }
}
