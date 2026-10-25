//! The database half of the statistics: plain counts and lists read straight
//! from the tables. All the sums, dates and streaks are worked out from these
//! by [`crate::stats`]; this file only asks SQLite.
//!
//! Nothing here changes the database, and no migration is needed: every number
//! comes from columns that already exist (`read`, `read_at`, `saved`, `note`,
//! `topic`, `source`) and from the `read_marks` table.
//!
//! **Combining devices.** An article opened on another device arrives as a read
//! mark (only if the person turned read-marks sync on). If the article has been
//! downloaded here too, the mark just flips `read` to 1 on it, so it is counted
//! once, as an article. If it has not been downloaded here, the mark waits in
//! `read_marks` with no article; those are counted separately
//! ([`StatCounts::marks_without_article`]) so a read on another device still
//! counts. Marks carry no topic or source, so they appear in the totals and the
//! charts but not in the top topics and sources.

#![warn(clippy::missing_docs_in_private_items)]

use super::Storage;

/// Plain counts used by the Stats screens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StatCounts {
    /// Articles on this device marked read.
    pub read_articles: u32,
    /// Read marks that arrived for articles this device does not have (opened
    /// on another device). Always 0 when read-marks sync is off.
    pub marks_without_article: u32,
    /// Articles not read and not skipped, the number the sidebar calls unread.
    pub unread: u32,
    /// Articles the person saved.
    pub saved: u32,
    /// Articles with a note (a note of only blank space does not count).
    pub notes: u32,
}

impl Storage {
    /// The counts behind the summary cards, in one go.
    pub(crate) fn stat_counts(&self) -> anyhow::Result<StatCounts> {
        // One small query per number keeps each readable; the tables are small
        // enough that this is instant.
        let count = |sql: &str| -> anyhow::Result<u32> {
            let n: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
            Ok(n.max(0) as u32)
        };
        Ok(StatCounts {
            read_articles: count("SELECT COUNT(*) FROM articles WHERE read = 1")?,
            marks_without_article: count(
                "SELECT COUNT(*) FROM read_marks m
                 WHERE NOT EXISTS (SELECT 1 FROM articles a WHERE a.url = m.key)",
            )?,
            unread: count("SELECT COUNT(*) FROM articles WHERE read = 0 AND skipped = 0")?,
            saved: count("SELECT COUNT(*) FROM articles WHERE saved = 1")?,
            notes: count(
                "SELECT COUNT(*) FROM articles WHERE note IS NOT NULL AND TRIM(note) <> ''",
            )?,
        })
    }

    /// When things were read, as milliseconds since 1970 (UTC), for the weekly
    /// chart and the streak: reads on this device plus marks from other devices
    /// whose article is not here.
    ///
    /// Articles read before migration 011 were given a guessed time equal to
    /// their publish time (see that migration). Those are not real reading
    /// times, so an article whose `read_at` is exactly its publish time is left
    /// out: a real read landing on the very same second as publication is
    /// vanishingly rare. That is what lets the chart and streak start from when
    /// real read times began being recorded.
    pub(crate) fn stat_read_times(&self) -> anyhow::Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT read_at FROM articles
             WHERE read = 1 AND read_at IS NOT NULL
               AND read_at <> CAST(strftime('%s', timestamp) AS INTEGER) * 1000
             UNION ALL
             SELECT m.read_at FROM read_marks m
             WHERE NOT EXISTS (SELECT 1 FROM articles a WHERE a.url = m.key)",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<i64>>>()
            .map_err(Into::into)
    }

    /// The topics (`by_topic = true`) or sources (`false`) with the most
    /// articles **read**, most first, at most `limit` of them. Ties break by
    /// name so the order is stable.
    pub(crate) fn stat_top_read(
        &self,
        by_topic: bool,
        limit: u32,
    ) -> anyhow::Result<Vec<(String, u32)>> {
        // The column name is one of two fixed words chosen here, never text
        // from outside, so building the SQL with `format!` is safe.
        let column = if by_topic { "topic" } else { "source" };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {column}, COUNT(*) AS n FROM articles WHERE read = 1
             GROUP BY {column} ORDER BY n DESC, {column} COLLATE NOCASE LIMIT ?1"
        ))?;
        let rows = stmt.query_map([limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u32))
        })?;
        rows.collect::<rusqlite::Result<Vec<(String, u32)>>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Article;
    use crate::stats::{NameCount, compute_stats};
    use chrono::{DateTime, TimeZone, Utc};

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

    /// Builds a database with a mix of articles and checks the cards: totals
    /// count every read article, guessed times are ignored by the chart, notes
    /// and saves are counted, and the top lists rank by articles read.
    #[test]
    fn stats_from_a_database() {
        let storage = Storage::open_in_memory().unwrap();
        let add = |n: u32, source: &str, topic: &str| {
            let mut article = Article::new(
                format!("title {n}"),
                format!("https://example.com/{n}"),
                source.to_string(),
                topic.to_string(),
                Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            );
            article.feed_url = Some("https://example.com/feed".to_string());
            storage.insert_article(&article).unwrap()
        };
        let a = add(1, "Arch", "linux");
        let b = add(2, "Arch", "linux");
        let c = add(3, "Overtake", "sim-racing");
        let _d = add(4, "Overtake", "sim-racing"); // stays unread

        // Three reads. `a` and `b` get real read times (written directly, since
        // `mark_read` stamps the real clock); `c` is read with a guessed time
        // equal to its publish time, like a pre-migration read.
        storage.mark_read(a).unwrap();
        storage.mark_read(b).unwrap();
        storage.mark_read(c).unwrap();
        let publish_ms = Utc
            .with_ymd_and_hms(2026, 10, 1, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        let set_time = |id: i64, value: i64| {
            storage
                .conn
                .execute(
                    "UPDATE articles SET read_at = ?2 WHERE id = ?1",
                    rusqlite::params![id, value],
                )
                .unwrap();
        };
        set_time(a, ms(2026, 10, 24, 9));
        set_time(b, ms(2026, 10, 25, 9));
        set_time(c, publish_ms);

        // One save with a note, one blank note that must not count.
        storage
            .save_article(a, Some("worth keeping".to_string()))
            .unwrap();
        storage
            .conn
            .execute(
                "UPDATE articles SET note = '   ' WHERE id = ?1",
                rusqlite::params![b],
            )
            .unwrap();

        let stats = compute_stats(&storage, now(), 0, 37).unwrap();
        assert_eq!(stats.read_total, 3);
        assert_eq!(stats.unread, 1);
        assert_eq!(stats.feeds, 37);
        assert_eq!(stats.saved, 1);
        assert_eq!(stats.notes, 1);
        // The guessed time is ignored: only two reads are dated.
        assert_eq!(stats.read_this_week, 2);
        assert_eq!((stats.streak_current, stats.streak_best), (2, 2));
        assert_eq!(stats.tracking_since.as_deref(), Some("2026-10-24"));
        // Ranked by articles read: linux 2, sim-racing 1.
        assert_eq!(
            stats.top_topics[0],
            NameCount {
                name: "linux".into(),
                reads: 2
            }
        );
        assert_eq!(
            stats.top_sources[1],
            NameCount {
                name: "Overtake".into(),
                reads: 1
            }
        );
    }

    /// A read mark that arrived for an article this device does not have still
    /// counts toward the totals and the chart.
    #[test]
    fn marks_from_other_devices_add_up() {
        let storage = Storage::open_in_memory().unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO read_marks (key, read_at, rev) VALUES (?1, ?2, '1-a')",
                rusqlite::params!["https://example.com/elsewhere", ms(2026, 10, 25, 8)],
            )
            .unwrap();
        let stats = compute_stats(&storage, now(), 0, 0).unwrap();
        assert_eq!(stats.read_total, 1);
        assert_eq!(stats.read_this_week, 1);
        assert_eq!(stats.streak_current, 1);
    }
}
