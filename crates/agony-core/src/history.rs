//! The run history model (Part07 7.4): the lines AGony itself executed,
//! newest last, capped, with consecutive duplicates collapsing. The
//! shell owns the file (`<config>/agony/history.json`) and the clock —
//! timestamps arrive as parameters, per the freedom grep.
//!
//! Items: `Entry` — one executed line (text, context cwd, timestamp ms,
//! exit status — `None` means success or not-yet-known); `History` —
//! the ordered list plus the queries.
//! Core functions: `History::record` — append with dedup-on-consecutive
//! and the cap applied; `History::completions` — the entries whose line
//! extends a typed prefix, newest first (the bar's history source);
//! `History::set_status` — the exit watcher's write-back, matched by
//! line and timestamp.
//! Depends on: serde (the JSON the shell persists).
//! Called by: the shell's `commandbar` (record + suggestions) and the
//! `agony history` verb.
//! Calls: nothing.

use serde::{Deserialize, Serialize};

/// The cap: entries beyond this drop oldest-first (Part07 7.4.1).
pub const MAX_ENTRIES: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The line exactly as executed.
    pub line: String,
    /// The working directory the bar showed as its context.
    pub cwd: String,
    /// Milliseconds since the shell's epoch (injected, never read here).
    pub timestamp_ms: u64,
    /// The exit status when the command failed; `None` on success or
    /// while unknown (Part07 7.4.2 — the memory of failures).
    #[serde(default)]
    pub status: Option<i32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    /// Oldest first; `record` keeps it that way.
    pub entries: Vec<Entry>,
}

impl History {
    /// Append an entry, collapsing a consecutive duplicate (same line
    /// and cwd as the last entry replaces it — the rerun updates its
    /// timestamp instead of doubling the list), then apply the cap.
    pub fn record(&mut self, entry: Entry) {
        if let Some(last) = self.entries.last_mut() {
            if last.line == entry.line && last.cwd == entry.cwd {
                *last = entry;
                return;
            }
        }
        self.entries.push(entry);
        self.cap();
    }

    /// Drop the oldest entries beyond [`MAX_ENTRIES`].
    pub fn cap(&mut self) {
        let excess = self.entries.len().saturating_sub(MAX_ENTRIES);
        if excess > 0 {
            self.entries.drain(..excess);
        }
    }

    /// The entries whose line extends `prefix` (and is not equal to
    /// it), newest first so earlier indexes rank as more recent.
    pub fn completions(&self, prefix: &str) -> Vec<&Entry> {
        self.entries
            .iter()
            .rev()
            .filter(|entry| entry.line.starts_with(prefix) && entry.line != prefix)
            .collect()
    }

    /// Record a finished command's exit status on the entry it belongs
    /// to (line + timestamp identify it; the watcher's write-back).
    pub fn set_status(&mut self, line: &str, timestamp_ms: u64, status: i32) {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.line == line && entry.timestamp_ms == timestamp_ms)
        else {
            return;
        };
        entry.status = Some(status);
    }

    /// Whether any entry failed (the CLI's exit-code decision).
    pub fn has_failures(&self) -> bool {
        self.entries.iter().any(|entry| entry.status.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(line: &str, cwd: &str, at: u64) -> Entry {
        Entry {
            line: line.into(),
            cwd: cwd.into(),
            timestamp_ms: at,
            status: None,
        }
    }

    #[test]
    fn record_appends_in_order() {
        let mut history = History::default();
        history.record(entry("git status", r"C:\repo", 1));
        history.record(entry("cargo test", r"C:\repo", 2));
        let lines: Vec<&str> = history.entries.iter().map(|e| e.line.as_str()).collect();
        assert_eq!(lines, ["git status", "cargo test"]);
    }

    #[test]
    fn consecutive_duplicates_replace_the_last_entry() {
        let mut history = History::default();
        history.record(entry("git status", r"C:\repo", 1));
        history.record(entry("git status", r"C:\repo", 2));
        history.record(entry("git status", r"C:\other", 3));
        assert_eq!(history.entries.len(), 2, "same line+cwd collapses");
        assert_eq!(history.entries[0].timestamp_ms, 2, "the rerun updated it");
        assert_eq!(history.entries[1].line, "git status", "other cwd survives");
    }

    #[test]
    fn cap_drops_oldest_beyond_the_limit() {
        let mut history = History::default();
        for at in 0..(MAX_ENTRIES + 5) as u64 {
            history.record(entry(&format!("line {at}"), "cwd", at));
        }
        assert_eq!(history.entries.len(), MAX_ENTRIES);
        assert_eq!(history.entries[0].line, "line 5", "the five oldest dropped");
        assert_eq!(
            history.entries.last().map(|e| e.line.as_str()),
            Some("line 1004")
        );
    }

    #[test]
    fn completions_extend_the_prefix_newest_first() {
        let mut history = History::default();
        history.record(entry("git status", "cwd", 1));
        history.record(entry("git checkout main", "cwd", 2));
        history.record(entry("cargo test", "cwd", 3));
        let lines: Vec<String> = history
            .completions("git ")
            .into_iter()
            .map(|entry| entry.line.clone())
            .collect();
        assert_eq!(lines, ["git checkout main", "git status"], "newest first");
        assert!(
            history.completions("git status").is_empty(),
            "equal is not a continuation"
        );
        assert!(history.completions("zzz").is_empty());
    }

    #[test]
    fn set_status_targets_line_and_timestamp() {
        let mut history = History::default();
        history.record(entry("failing one", "cwd", 10));
        history.record(entry("failing two", "cwd", 20));
        history.set_status("failing two", 20, 1);
        assert_eq!(history.entries[0].status, None);
        assert_eq!(history.entries[1].status, Some(1));
        history.set_status("absent", 1, 2);
        assert_eq!(history.entries[0].status, None, "no entry, no crash");
    }

    #[test]
    fn history_json_round_trips() {
        let mut history = History::default();
        history.record(Entry {
            line: "git status".into(),
            cwd: r"C:\repo".into(),
            timestamp_ms: 5,
            status: Some(128),
        });
        let text = serde_json::to_string(&history).expect("serializes");
        let parsed: History = serde_json::from_str(&text).expect("parses");
        assert_eq!(parsed, history);
        // An old file without `status` loads with None.
        let legacy: History =
            serde_json::from_str(r#"{"entries":[{"line":"ls","cwd":".","timestamp_ms":1}]}"#)
                .expect("legacy loads");
        assert_eq!(legacy.entries[0].status, None);
    }

    #[test]
    fn has_failures_reflects_any_status() {
        let mut history = History::default();
        assert!(!history.has_failures());
        history.record(entry("ok", "cwd", 1));
        assert!(!history.has_failures());
        history.set_status("ok", 1, 3);
        assert!(history.has_failures());
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    fn record_lines(history: &mut History, records: &[(String, String)]) {
        for (index, (line, cwd)) in records.iter().enumerate() {
            history.record(entry(line, cwd, index as u64 + 1));
        }
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// Recording stays capped, consecutively unique and time-ordered
        /// under any record stream.
        #[test]
        fn invariant_random_records_stay_capped_and_ordered(
            records in proptest::collection::vec(("[a-z]{0,8}", "[a-b]"), 0..1100),
        ) {
            let mut history = History::default();
            record_lines(&mut history, &records);
            prop_assert!(history.entries.len() <= MAX_ENTRIES, "cap leaked");
            let duplicated = history
                .entries
                .windows(2)
                .any(|pair| pair[0].line == pair[1].line && pair[0].cwd == pair[1].cwd);
            prop_assert!(!duplicated, "consecutive duplicates survive");
            let ordered = history
                .entries
                .windows(2)
                .all(|pair| pair[0].timestamp_ms < pair[1].timestamp_ms);
            prop_assert!(ordered, "timestamps left input order");
        }

        /// completions is exactly the reversed prefix filter.
        #[test]
        fn prop_completions_are_the_reversed_prefix_filter(
            records in proptest::collection::vec(("[a-z]{0,8}", "[a-b]"), 0..40),
            prefix in "[a-z]{0,3}",
        ) {
            let mut history = History::default();
            record_lines(&mut history, &records);
            let got: Vec<String> = history
                .completions(&prefix)
                .into_iter()
                .map(|e| e.line.clone())
                .collect();
            let expected: Vec<String> = history
                .entries
                .iter()
                .rev()
                .filter(|e| e.line.starts_with(&prefix) && e.line != prefix)
                .map(|e| e.line.clone())
                .collect();
            prop_assert_eq!(got, expected);
        }

        /// The JSON round trip preserves any history, statuses included.
        #[test]
        fn prop_history_json_round_trips(
            records in proptest::collection::vec(("[a-z]{0,8}", "[a-b]", any::<Option<i32>>()), 0..40),
        ) {
            let mut history = History::default();
            for (index, (line, cwd, status)) in records.into_iter().enumerate() {
                history.record(Entry {
                    line,
                    cwd,
                    timestamp_ms: index as u64 + 1,
                    status,
                });
            }
            let text = serde_json::to_string(&history).expect("serializes");
            let parsed: History = serde_json::from_str(&text).expect("parses");
            prop_assert_eq!(parsed, history);
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    #[test]
    fn set_status_needs_line_and_timestamp_together() {
        let mut history = History::default();
        history.record(entry("dup", "a", 10));
        history.record(entry("other", "b", 11));
        history.record(entry("dup", "a", 20));
        history.set_status("dup", 20, 1);
        assert_eq!(history.entries[0].status, None, "the first run stays clean");
        assert_eq!(history.entries[1].status, None);
        assert_eq!(history.entries[2].status, Some(1));
    }
}
