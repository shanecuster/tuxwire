//! Reading statistics: the numbers behind the Stats screens of the terminal app
//! and the Android app.
//!
//! Everything is worked out **once, here**, and handed back as one plain
//! [`Stats`] value, so the two apps only have to lay the numbers out. That keeps
//! them from ever disagreeing about, say, what a streak is.
//!
//! The numbers come from data that already exists (no migration): the read,
//! saved and note columns, `read_at` (when an article was opened), and, when
//! read-marks sync is on, the marks that arrived from the person's other
//! devices, so reading on a phone and on a computer adds up.
//!
//! **Where the history starts.** Reading times were only recorded from the
//! build that added `read_at`; older reads were given their publish date as a
//! guess, which is not a real reading time and is ignored here. So the weekly
//! chart and the streak start from when real times began, and `tracking_since`
//! says when. The totals still count every read article.
//!
//! **Time zones.** What counts as "today" or "this week" depends on where the
//! person is, but this crate never reads the environment, so the app passes in
//! the current time and its offset from UTC.

#![warn(clippy::missing_docs_in_private_items)]

use crate::storage::Storage;
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use std::collections::BTreeSet;

/// How many weeks the weekly chart covers, including the current one.
pub const CHART_WEEKS: usize = 12;

/// How many entries the top-topics and top-sources lists hold at most.
pub const TOP_COUNT: u32 = 5;

/// A name with the number of articles read from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameCount {
    /// The topic or source name.
    pub name: String,
    /// How many of its articles were read.
    pub reads: u32,
}

/// Reads in one week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeekCount {
    /// The Monday that starts the week, as `YYYY-MM-DD`.
    pub week_start: String,
    /// How many articles were read that week.
    pub reads: u32,
}

/// Every number the Stats screens show. Plain data: no behaviour, so it can be
/// passed to Kotlin unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Articles read in all, this device's plus those opened on other devices
    /// (needs read-marks sync). Counts reads from before times were recorded.
    pub read_total: u32,
    /// Reads since the Monday of this week (local time).
    pub read_this_week: u32,
    /// Reads since the first of this month (local time).
    pub read_this_month: u32,
    /// Articles not read and not skipped, on this device.
    pub unread: u32,
    /// Feeds across everything, as counted by the app that asked.
    pub feeds: u32,
    /// Articles with a note.
    pub notes: u32,
    /// Articles saved.
    pub saved: u32,
    /// Days in a row, up to today, with at least one read. Today may still be
    /// empty: a streak that reached yesterday is not broken until the day ends.
    pub streak_current: u32,
    /// The longest run of such days since times began being recorded.
    pub streak_best: u32,
    /// Reads per week for the last [`CHART_WEEKS`] weeks, oldest first.
    pub weeks: Vec<WeekCount>,
    /// The date (`YYYY-MM-DD`) of the earliest real read time, or `None` when
    /// none has been recorded yet. The chart and streak start here.
    pub tracking_since: Option<String>,
    /// Topics with the most articles read, most first.
    pub top_topics: Vec<NameCount>,
    /// Sources with the most articles read, most first.
    pub top_sources: Vec<NameCount>,
}

/// What can be worked out from reading times alone. Split out from [`Stats`] so
/// the date arithmetic can be tested without a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadSummary {
    /// Reads since this week's Monday.
    pub this_week: u32,
    /// Reads since the first of this month.
    pub this_month: u32,
    /// The current streak (see [`Stats::streak_current`]).
    pub streak_current: u32,
    /// The best streak (see [`Stats::streak_best`]).
    pub streak_best: u32,
    /// The weekly chart, oldest week first.
    pub weeks: Vec<WeekCount>,
    /// The date of the earliest read, as `YYYY-MM-DD`.
    pub since: Option<String>,
}

/// Works out the weekly counts, the streaks and the period totals from a list
/// of reading times.
///
/// - `read_times_ms` are moments in milliseconds since 1970 (UTC), in any order.
/// - `now` is the current moment, and `utc_offset_secs` how far the person's
///   clock is ahead of UTC (negative west of it, such as -14400 for US Eastern
///   daylight time). Both decide which calendar day a read falls on.
///
/// `&[i64]` borrows the list without copying it.
pub(crate) fn summarize_reads(
    read_times_ms: &[i64],
    now: DateTime<Utc>,
    utc_offset_secs: i32,
) -> ReadSummary {
    // Step 1: the person's clock. An offset outside the legal range falls back
    // to UTC rather than failing.
    let zone = FixedOffset::east_opt(utc_offset_secs).unwrap_or(FixedOffset::east_opt(0).unwrap());

    // Step 2: turn each moment into the local calendar day it fell on. A moment
    // that cannot be represented (absurdly far away) is skipped.
    let days: Vec<NaiveDate> = read_times_ms
        .iter()
        .filter_map(|ms| zone.timestamp_millis_opt(*ms).single())
        .map(|moment| moment.date_naive())
        .collect();

    // Step 3: today, this week's Monday and the first of the month, all local.
    let today = now.with_timezone(&zone).date_naive();
    let week_start = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let month_start = today.with_day(1).unwrap_or(today);

    // Step 4: reads this week and this month. Future-dated times (a clock set
    // wrong on another device) are not counted as "this week".
    let this_week = days
        .iter()
        .filter(|day| **day >= week_start && **day <= today)
        .count() as u32;
    let this_month = days
        .iter()
        .filter(|day| **day >= month_start && **day <= today)
        .count() as u32;

    // Step 5: the weekly chart. Walk back from this week's Monday one week at a
    // time, so the list ends with the current week, then flip it so it reads
    // oldest first.
    let mut weeks = Vec::with_capacity(CHART_WEEKS);
    for back in 0..CHART_WEEKS as i64 {
        let start = week_start - Duration::days(7 * back);
        let end = start + Duration::days(6);
        let reads = days
            .iter()
            .filter(|day| **day >= start && **day <= end)
            .count() as u32;
        weeks.push(WeekCount {
            week_start: start.format("%Y-%m-%d").to_string(),
            reads,
        });
    }
    weeks.reverse();

    // Step 6: streaks, from the distinct days that had a read. A `BTreeSet` is a
    // sorted set, so walking it goes oldest to newest with no repeats.
    let distinct: BTreeSet<NaiveDate> = days.iter().copied().filter(|day| *day <= today).collect();

    // Best: the longest run of consecutive days anywhere.
    let mut streak_best = 0u32;
    let mut run = 0u32;
    let mut previous: Option<NaiveDate> = None;
    for day in &distinct {
        run = match previous {
            Some(prev) if *day - prev == Duration::days(1) => run + 1,
            _ => 1,
        };
        streak_best = streak_best.max(run);
        previous = Some(*day);
    }

    // Current: count back from today, or from yesterday when today has no read
    // yet (the day is not over, so the streak is still alive).
    let mut cursor = if distinct.contains(&today) {
        today
    } else {
        today - Duration::days(1)
    };
    let mut streak_current = 0u32;
    while distinct.contains(&cursor) {
        streak_current += 1;
        cursor -= Duration::days(1);
    }

    ReadSummary {
        this_week,
        this_month,
        streak_current,
        streak_best,
        weeks,
        since: distinct
            .iter()
            .next()
            .map(|day| day.format("%Y-%m-%d").to_string()),
    }
}

/// Computes every statistic.
///
/// - `now` and `utc_offset_secs`: the current moment and the person's offset
///   from UTC, see [`summarize_reads`].
/// - `feeds`: how many feeds there are across everything. Each app counts its
///   own (the terminal app reads its feed list files, Android its feed table),
///   so it is passed in rather than guessed here.
///
/// Reads the database only; nothing is written.
pub fn compute_stats(
    storage: &Storage,
    now: DateTime<Utc>,
    utc_offset_secs: i32,
    feeds: u32,
) -> anyhow::Result<Stats> {
    // Step 1: the plain counts and the list of reading times.
    let counts = storage.stat_counts()?;
    let times = storage.stat_read_times()?;

    // Step 2: the date arithmetic.
    let summary = summarize_reads(&times, now, utc_offset_secs);

    // Step 3: the top lists, by articles read.
    let to_names = |rows: Vec<(String, u32)>| -> Vec<NameCount> {
        rows.into_iter()
            .map(|(name, reads)| NameCount { name, reads })
            .collect()
    };
    let top_topics = to_names(storage.stat_top_read(true, TOP_COUNT)?);
    let top_sources = to_names(storage.stat_top_read(false, TOP_COUNT)?);

    Ok(Stats {
        read_total: counts.read_articles + counts.marks_without_article,
        read_this_week: summary.this_week,
        read_this_month: summary.this_month,
        unread: counts.unread,
        feeds,
        notes: counts.notes,
        saved: counts.saved,
        streak_current: summary.streak_current,
        streak_best: summary.streak_best,
        weeks: summary.weeks,
        tracking_since: summary.since,
        top_topics,
        top_sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A UTC moment from a date and time, as milliseconds since 1970.
    fn ms(y: i32, m: u32, d: u32, h: u32) -> i64 {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0)
            .unwrap()
            .timestamp_millis()
    }

    /// "Now" for the tests: Sunday 25 October 2026, noon UTC.
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap()
    }

    /// With no reads there is nothing: zero streaks, an empty-but-complete
    /// chart, no start date.
    #[test]
    fn nothing_read_gives_empty_summary() {
        let summary = summarize_reads(&[], now(), 0);
        assert_eq!((summary.streak_current, summary.streak_best), (0, 0));
        assert_eq!(summary.weeks.len(), CHART_WEEKS);
        assert!(summary.weeks.iter().all(|week| week.reads == 0));
        assert_eq!(summary.since, None);
    }

    /// The chart ends with the current week (Monday 2026-10-19) and counts reads
    /// into the right weeks; week and month totals follow.
    #[test]
    fn weeks_and_period_totals() {
        let times = [
            ms(2026, 10, 25, 9), // Sunday, this week
            ms(2026, 10, 19, 9), // Monday, this week
            ms(2026, 10, 18, 9), // Sunday, last week
            ms(2026, 9, 30, 9),  // last month
        ];
        let summary = summarize_reads(&times, now(), 0);
        assert_eq!(summary.this_week, 2);
        assert_eq!(summary.this_month, 3);
        let last = summary.weeks.last().unwrap();
        assert_eq!((last.week_start.as_str(), last.reads), ("2026-10-19", 2));
        let before = &summary.weeks[CHART_WEEKS - 2];
        assert_eq!(
            (before.week_start.as_str(), before.reads),
            ("2026-10-12", 1)
        );
        assert_eq!(summary.since.as_deref(), Some("2026-09-30"));
    }

    /// A streak counts consecutive days ending today; an empty today does not
    /// break a streak that reached yesterday; a gap does.
    #[test]
    fn streaks() {
        // Read on the 23rd, 24th and 25th: a 3-day streak, alive today.
        let alive = summarize_reads(
            &[
                ms(2026, 10, 23, 8),
                ms(2026, 10, 24, 8),
                ms(2026, 10, 25, 8),
            ],
            now(),
            0,
        );
        assert_eq!((alive.streak_current, alive.streak_best), (3, 3));

        // Nothing yet today, but the 23rd and 24th: still 2.
        let waiting = summarize_reads(&[ms(2026, 10, 23, 8), ms(2026, 10, 24, 8)], now(), 0);
        assert_eq!(waiting.streak_current, 2);

        // Last read two days ago: the streak is over, the best is remembered.
        let broken = summarize_reads(
            &[
                ms(2026, 10, 21, 8),
                ms(2026, 10, 22, 8),
                ms(2026, 10, 23, 8),
            ],
            now(),
            0,
        );
        assert_eq!(broken.streak_current, 0);
        assert_eq!(broken.streak_best, 3);
    }

    /// The same moment falls on different days in different time zones.
    #[test]
    fn the_offset_decides_which_day_a_read_is_on() {
        // 02:00 UTC on the 25th is still the 24th at US Eastern (UTC-4).
        let times = [ms(2026, 10, 25, 2)];
        let utc = summarize_reads(&times, now(), 0);
        let eastern = summarize_reads(&times, now(), -4 * 3600);
        assert_eq!(utc.since.as_deref(), Some("2026-10-25"));
        assert_eq!(eastern.since.as_deref(), Some("2026-10-24"));
    }
}
