//! Fuzzy ranking and dedup — the start menu's filter today, the command
//! bar's engine in Part07, the file panel's search in Part05. Written
//! once.
//!
//! Items: `Suggestion` — one ranked item carrying its original index;
//! `FieldItem` — one item with the fields the filter may match;
//! `PathItem` — one file-panel entry (label plus full path);
//! `Candidate` — one command-bar completion (insertion text, display,
//! source); `Source` — where a candidate came from, in merge-priority
//! order.
//! Core functions: `rank` — match over the label only (the two-field
//! API); `rank_fields` — match over any of several fields, best field
//! wins (the start menu matches name, description and category through
//! it); `rank_paths` — the path-aware form (deeper paths rank lower at
//! equal score — Part05 5.4.2); `merge_ranked` — the bar's merge:
//! score desc, then list order (the lists arrive history > carapace >
//! catalog > files), then each list's own order (recency for history),
//! dedup on the insertion text (Part07 7.3.3); `match_positions` — where the
//! query's characters landed in a text, for highlighting. All share the
//! same scoring; each has its own dedup and tiebreak.
//! Depends on: nothing.
//! Called by: the shell's start menu, file panel; Part07's command bar.
//! Calls: nothing outside this crate.

use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub hint: String,
    pub index: usize,
    pub score: i64,
}

/// One item: the displayed label and hint, plus every field the filter
/// may match against (the label is usually one of them).
#[derive(Clone, Debug)]
pub struct FieldItem {
    pub label: String,
    pub hint: String,
    pub fields: Vec<String>,
}

/// One file-panel search entry: the file's name (label) and its joined
/// path from the panel's root (the match target).
#[derive(Clone, Debug)]
pub struct PathItem {
    pub label: String,
    pub path: String,
}

/// Rank `items` against `query`: case-insensitive subsequence match, score
/// desc then label asc then original index; duplicate labels keep the first
/// (best) one. An empty query returns everything unranked, in order.
/// Matches against the label only — `rank_fields` is the general form.
pub fn rank(query: &str, items: &[(String, String)]) -> Vec<Suggestion> {
    let fields: Vec<FieldItem> = items
        .iter()
        .map(|(label, hint)| FieldItem {
            label: label.clone(),
            hint: hint.clone(),
            fields: vec![label.clone()],
        })
        .collect();
    rank_fields(query, &fields)
}

/// The same ranking, but a query may match any of the item's fields; the
/// best field's score counts.
pub fn rank_fields(query: &str, items: &[FieldItem]) -> Vec<Suggestion> {
    if query.is_empty() {
        return dedup_by_label(unranked(items));
    }
    let matched = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let score = best_score(query, &item.fields)?;
            Some(Suggestion {
                label: item.label.clone(),
                hint: item.hint.clone(),
                index,
                score,
            })
        })
        .collect();
    let mut sorted: Vec<Suggestion> = matched;
    sorted.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.index.cmp(&b.index))
    });
    dedup_by_label(sorted)
}

fn unranked(items: &[FieldItem]) -> Vec<Suggestion> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| Suggestion {
            label: item.label.clone(),
            hint: item.hint.clone(),
            index,
            score: 0,
        })
        .collect()
}

fn best_score(query: &str, fields: &[String]) -> Option<i64> {
    fields
        .iter()
        .filter_map(|field| fuzzy_score(query, field))
        .max()
}

/// The path-aware ranking (Part05 5.4.2): fuzzy match over the full
/// path, score desc, then shallower paths first (fewer separators), then
/// label asc, then original index. Dedup is by path — two files may
/// share a name in different directories and both must survive.
pub fn rank_paths(query: &str, items: &[PathItem]) -> Vec<Suggestion> {
    let matched: Vec<Suggestion> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let score = fuzzy_score(query, &item.path)?;
            Some(Suggestion {
                label: item.label.clone(),
                hint: item.path.clone(),
                index,
                score,
            })
        })
        .collect();
    let mut sorted = matched;
    sorted.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| depth_of(&a.hint).cmp(&depth_of(&b.hint)))
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.index.cmp(&b.index))
    });
    dedup_by_hint(sorted)
}

fn depth_of(path: &str) -> usize {
    path.chars().filter(|c| *c == '\\' || *c == '/').count()
}

// ------------------------------------------------- command-bar merge

/// Where a command-bar candidate came from — display metadata. The
/// merge priority is positional: `merge_ranked`'s lists arrive history,
/// carapace, catalog, files (Part07 7.3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    History,
    Carapace,
    Catalog,
    Files,
}

/// Whether a candidate replaces the active word or the whole line
/// (history's completing continuations replace the line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Word,
    Line,
}

/// One command-bar completion after per-source ranking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The exact text inserted at the cursor (quoting included).
    pub insert: String,
    /// The row's left text.
    pub display: String,
    /// The row's right-aligned dim text.
    pub description: String,
    pub source: Source,
    pub scope: Scope,
    /// True for a recorded failure (the dim `✗` row, Part07 7.4.2).
    pub failed: bool,
}

impl Candidate {
    pub fn word(insert: impl Into<String>, source: Source) -> Candidate {
        Candidate {
            insert: insert.into(),
            display: String::new(),
            description: String::new(),
            source,
            scope: Scope::Word,
            failed: false,
        }
    }

    /// Display falls back to the insertion text (the usual case).
    pub fn shown(mut self) -> Candidate {
        if self.display.is_empty() {
            self.display = self.insert.clone();
        }
        self
    }
}

/// The one merge (Part07 7.3.3): the four lists arrive in
/// merge-priority order (history, carapace, catalog, files) already
/// ranked by the engine (`rank` / `rank_paths`, scores kept in
/// [`Ranked`]), and this interleaves them — score desc first, then list
/// order (which *is* the source priority by position), then each list's
/// own order (history passes newest first, so its index is recency) —
/// deduplicating on the insertion text (the first, best candidate
/// wins). The candidates' `Source` field is display metadata; the
/// position decides.
pub fn merge_ranked(lists: [Vec<Ranked>; 4]) -> Vec<Candidate> {
    let mut tagged: Vec<(usize, Ranked)> = Vec::new();
    for (priority, list) in lists.iter().enumerate() {
        tagged.extend(list.iter().cloned().map(|ranked| (priority, ranked)));
    }
    tagged.sort_by(|(pa, a), (pb, b)| b.score.cmp(&a.score).then_with(|| pa.cmp(pb)));
    let mut seen: HashSet<String> = HashSet::new();
    let mut merged = Vec::new();
    for (_, ranked) in tagged {
        let insert = ranked.candidate.insert.clone();
        if seen.insert(insert) {
            merged.push(ranked.candidate);
        }
    }
    merged
}

/// A candidate with the score its source's ranking gave it (the merge's
/// input unit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranked {
    pub candidate: Candidate,
    pub score: i64,
}

/// Where the query's characters landed in `text` (the greedy subsequence
/// the scorer uses), for highlighting the matched characters — None when
/// there is no match.
pub fn match_positions(query: &str, text: &str) -> Option<Vec<usize>> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut positions = Vec::with_capacity(q.len());
    let mut search_from = 0usize;
    for &wanted in &q {
        let found = t[search_from..].iter().position(|c| *c == wanted)? + search_from;
        positions.push(found);
        search_from = found + 1;
    }
    Some(positions)
}

fn dedup_by_label(list: Vec<Suggestion>) -> Vec<Suggestion> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique = list;
    unique.retain(|suggestion| seen.insert(suggestion.label.clone()));
    unique
}

/// Path dedup: the hint (the joined path) is the identity, so equal
/// names in different directories both survive.
fn dedup_by_hint(list: Vec<Suggestion>) -> Vec<Suggestion> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique = list;
    unique.retain(|suggestion| seen.insert(suggestion.hint.clone()));
    unique
}

/// Case-insensitive fuzzy score: None when `query` is not a subsequence of
/// `label`; otherwise higher is better (prefix, word starts and runs of
/// consecutive hits score; long labels lose a hair).
fn fuzzy_score(query: &str, label: &str) -> Option<i64> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let l: Vec<char> = label.to_lowercase().chars().collect();
    if q.is_empty() || q.len() > l.len() {
        return None;
    }
    let mut score = i64::from(l.starts_with(&q)) * 12;
    let mut consecutive = 0i64;
    let mut last_hit: Option<usize> = None;
    let mut search_from = 0usize;
    for &wanted in &q {
        let found = l[search_from..].iter().position(|c| *c == wanted)? + search_from;
        consecutive = if last_hit == Some(found.wrapping_sub(1)) {
            consecutive + 1
        } else {
            1
        };
        score += 1 + consecutive * 3 + word_start_bonus(&l, found);
        last_hit = Some(found);
        search_from = found + 1;
    }
    Some(score - l.len() as i64 / 8)
}

fn word_start_bonus(label: &[char], at: usize) -> i64 {
    if at == 0 {
        return 4;
    }
    let starts_word = matches!(label[at - 1], ' ' | '_' | '-' | '.' | '/' | '\\');
    i64::from(starts_word) * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<(String, String)> {
        vec![
            ("PowerShell".into(), "shell".into()),
            ("git".into(), "version control".into()),
            ("grep".into(), "search text".into()),
        ]
    }

    #[test]
    fn empty_query_returns_everything_in_order() {
        let ranked = rank("", &items());
        let labels: Vec<&str> = ranked.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["PowerShell", "git", "grep"]);
    }

    #[test]
    fn non_matching_query_returns_nothing() {
        assert!(rank("zzz", &items()).is_empty());
    }

    #[test]
    fn matching_is_case_insensitive() {
        let ranked = rank("POW", &items());
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].label, "PowerShell");
    }

    #[test]
    fn prefix_matches_outrank_scattered_ones() {
        let pair = vec![
            ("bash".to_string(), "shell".to_string()),
            ("shell".to_string(), "shell".to_string()),
        ];
        let ranked = rank("s", &pair);
        assert_eq!(ranked[0].label, "shell");
    }

    #[test]
    fn consecutive_hits_score_higher_than_scattered() {
        let pair = vec![
            ("c-a-t".to_string(), String::new()),
            ("cat".to_string(), String::new()),
        ];
        let ranked = rank("cat", &pair);
        assert_eq!(ranked[0].label, "cat");
    }

    #[test]
    fn duplicate_labels_keep_the_best_one() {
        let pair = vec![
            ("git".to_string(), "first".to_string()),
            ("git".to_string(), "second".to_string()),
        ];
        let ranked = rank("git", &pair);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].hint, "first");
    }

    #[test]
    fn ranked_items_carry_their_original_index() {
        let ranked = rank("git", &items());
        assert_eq!(ranked[0].index, 1);
    }

    fn field_items() -> Vec<FieldItem> {
        vec![
            FieldItem {
                label: "Glow".into(),
                hint: "markdown reader".into(),
                fields: vec![
                    "Glow".into(),
                    "Render markdown in the terminal".into(),
                    "text".into(),
                ],
            },
            FieldItem {
                label: "Superfile".into(),
                hint: "file manager".into(),
                fields: vec![
                    "Superfile".into(),
                    "terminal file manager".into(),
                    "files".into(),
                ],
            },
        ]
    }

    #[test]
    fn fields_match_the_description_and_the_category() {
        let ranked = rank_fields("markdown", &field_items());
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].label, "Glow");
        let ranked = rank_fields("files", &field_items());
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].label, "Superfile");
    }

    #[test]
    fn the_best_matching_field_scores() {
        // "glow" matches the label strongly, the description not at all;
        // "render" only matches the description — both surface the item.
        let ranked = rank_fields("glow", &field_items());
        assert_eq!(ranked[0].label, "Glow");
        let ranked = rank_fields("render", &field_items());
        assert_eq!(ranked[0].label, "Glow");
    }

    #[test]
    fn rank_stays_label_only() {
        // The two-field API must not start matching hints: "reader" is in
        // Glow's hint, not its label.
        let items = vec![("Glow".to_string(), "markdown reader".to_string())];
        assert!(rank("reader", &items).is_empty());
    }

    #[test]
    fn empty_query_over_fields_returns_everything_in_order() {
        let ranked = rank_fields("", &field_items());
        let labels: Vec<&str> = ranked.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["Glow", "Superfile"]);
    }

    // ------------------------------------------------------- path search

    fn path_items() -> Vec<PathItem> {
        vec![
            PathItem {
                label: "main.rs".into(),
                path: "src\\main.rs".into(),
            },
            PathItem {
                label: "main.rs".into(),
                path: "examples\\deep\\main.rs".into(),
            },
            PathItem {
                label: "notes.md".into(),
                path: "docs\\notes.md".into(),
            },
        ]
    }

    #[test]
    fn path_search_matches_over_the_full_path() {
        let ranked = rank_paths("srcma", &path_items());
        assert_eq!(
            ranked[0].hint, "src\\main.rs",
            "the path is the match field"
        );
        let by_name = rank_paths("notes", &path_items());
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].label, "notes.md");
    }

    #[test]
    fn path_search_prefers_shallower_paths_at_equal_score() {
        let ranked = rank_paths("main.rs", &path_items());
        assert_eq!(
            ranked[0].hint, "src\\main.rs",
            "equal scores rank by depth, shallower first"
        );
        assert_eq!(ranked[1].hint, "examples\\deep\\main.rs");
    }

    #[test]
    fn path_search_dedups_by_path_not_by_label() {
        let ranked = rank_paths("main", &path_items());
        let mains = ranked.iter().filter(|s| s.label == "main.rs").count();
        assert_eq!(mains, 2, "same name in two directories both survive");
    }

    #[test]
    fn match_positions_pin_the_greedy_subsequence() {
        assert_eq!(match_positions("mr", "main.rs"), Some(vec![0, 5]));
        assert_eq!(match_positions("MS", "main.rs"), Some(vec![0, 6]));
        assert_eq!(match_positions("zz", "main.rs"), None);
        assert_eq!(match_positions("", "main.rs"), Some(Vec::new()));
    }

    // ------------------------------------------------- command-bar merge

    fn candidate(insert: &str, source: Source, scope: Scope) -> Ranked {
        Ranked {
            candidate: Candidate {
                insert: insert.into(),
                display: insert.into(),
                description: String::new(),
                source,
                scope,
                failed: false,
            },
            score: 0,
        }
    }

    #[test]
    fn merge_ranks_by_score_then_source_then_recency() {
        let mut history = candidate("git checkout main", Source::History, Scope::Line);
        let mut carapace = candidate("checkout ", Source::Carapace, Scope::Word);
        let mut catalog = candidate("git", Source::Catalog, Scope::Word);
        catalog.score = 30;
        carapace.score = 20;
        history.score = 10;
        let merged = merge_ranked([vec![history], vec![carapace], vec![catalog], vec![]]);
        let inserts: Vec<&str> = merged.iter().map(|c| c.insert.as_str()).collect();
        assert_eq!(inserts, ["git", "checkout ", "git checkout main"]);
    }

    #[test]
    fn merge_uses_source_priority_for_equal_scores() {
        let merged = merge_ranked([
            // history, newest first
            vec![candidate("git checkout main", Source::History, Scope::Line)],
            vec![candidate("checkout ", Source::Carapace, Scope::Word)],
            vec![candidate("git", Source::Catalog, Scope::Word)],
            vec![],
        ]);
        let inserts: Vec<&str> = merged.iter().map(|c| c.insert.as_str()).collect();
        assert_eq!(
            inserts,
            ["git checkout main", "checkout ", "git"],
            "equal scores order history > carapace > catalog > files"
        );
    }

    #[test]
    fn merge_prefers_higher_score_over_source_priority() {
        let mut weak_history = candidate("h", Source::History, Scope::Line);
        let mut strong_files = candidate("f", Source::Files, Scope::Word);
        strong_files.score = 50;
        weak_history.score = 1;
        let merged = merge_ranked([vec![weak_history], vec![], vec![], vec![strong_files]]);
        assert_eq!(merged[0].insert, "f", "score outranks source priority");
    }

    #[test]
    fn merge_uses_recency_for_equal_history_scores() {
        let old = candidate("git status", Source::History, Scope::Line);
        let new = candidate("git checkout main", Source::History, Scope::Line);
        let merged = merge_ranked([vec![old, new], vec![], vec![], vec![]]);
        assert_eq!(
            merged[0].insert, "git status",
            "the caller passes newest first"
        );
        assert_eq!(merged[1].insert, "git checkout main");
    }

    #[test]
    fn merge_dedups_on_the_insertion_text_first_winner() {
        let carapace = candidate("checkout ", Source::Carapace, Scope::Word);
        let mut catalog = candidate("checkout ", Source::Catalog, Scope::Word);
        catalog.score = 100;
        let merged = merge_ranked([vec![], vec![carapace], vec![catalog], vec![]]);
        assert_eq!(merged.len(), 1);
        // Equal scores: history's copy wins the dedup over carapace's.
        let merged = merge_ranked([
            vec![candidate("checkout ", Source::History, Scope::Word)],
            vec![candidate("checkout ", Source::Carapace, Scope::Word)],
            vec![],
            vec![],
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, Source::History);
    }

    #[test]
    fn candidates_carry_scope_and_failure() {
        let mut failed = candidate("git status", Source::History, Scope::Line);
        failed.candidate.failed = true;
        let merged = merge_ranked([vec![failed], vec![], vec![], vec![]]);
        assert!(merged[0].failed);
        assert_eq!(merged[0].scope, Scope::Line);
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    /// The greedy subsequence, written out independently (the oracle for
    /// both the highlighter and the label matcher).
    fn subsequence_positions(query: &str, text: &str) -> Option<Vec<usize>> {
        let wanted: Vec<char> = query.to_lowercase().chars().collect();
        let have: Vec<char> = text.to_lowercase().chars().collect();
        let mut positions = Vec::new();
        let mut from = 0usize;
        for ch in wanted {
            let at = have.iter().skip(from).position(|c| *c == ch)? + from;
            positions.push(at);
            from = at + 1;
        }
        Some(positions)
    }

    fn ranked_of(insert: &str, source: Source, score: i64) -> Ranked {
        Ranked {
            candidate: Candidate::word(insert, source),
            score,
        }
    }

    fn merge_list_strategy(source: Source) -> impl Strategy<Value = Vec<Ranked>> {
        proptest::collection::vec((-30i64..30, "[a-z]{1,4}"), 0..6).prop_map(move |entries| {
            entries
                .into_iter()
                .map(|(score, insert)| ranked_of(&insert, source, score))
                .collect()
        })
    }

    /// The documented merge order, written out independently: score
    /// desc, then list order, deduped on insert.
    fn expected_merge(lists: &[Vec<Ranked>; 4]) -> Vec<(String, Source)> {
        let mut tagged: Vec<(usize, Ranked)> = lists
            .iter()
            .enumerate()
            .flat_map(|(priority, list)| {
                list.iter()
                    .cloned()
                    .map(move |ranked| (priority, ranked))
                    .collect::<Vec<_>>()
            })
            .collect();
        tagged.sort_by(|(pa, a), (pb, b)| b.score.cmp(&a.score).then_with(|| pa.cmp(pb)));
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (_, ranked) in tagged {
            if seen.insert(ranked.candidate.insert.clone()) {
                out.push((ranked.candidate.insert, ranked.candidate.source));
            }
        }
        out
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// match_positions is the greedy subsequence predicate, with
        /// strictly increasing hits.
        #[test]
        fn prop_match_positions_is_the_subsequence_predicate(
            query in "[a-zA-Z]{0,6}",
            text in "[a-zA-Z ]{0,20}",
        ) {
            prop_assert_eq!(
                match_positions(&query, &text),
                subsequence_positions(&query, &text)
            );
        }

        /// rank returns matching labels only, ordered score desc then
        /// label asc, deduped — and an empty query returns first
        /// occurrences in input order.
        #[test]
        fn prop_rank_orders_and_dedups(
            query in "[a-zA-Z]{0,4}",
            items in proptest::collection::vec(("[a-zA-Z]{0,10}", "[a-z]{0,8}"), 0..12),
        ) {
            let owned: Vec<(String, String)> = items;
            let ranked = rank(&query, &owned);
            if query.is_empty() {
                let mut seen = std::collections::HashSet::new();
                let expected: Vec<String> = owned
                    .iter()
                    .map(|(label, _)| label.clone())
                    .filter(|label| seen.insert(label.clone()))
                    .collect();
                let got: Vec<String> = ranked.iter().map(|s| s.label.clone()).collect();
                prop_assert_eq!(got, expected);
                return Ok(());
            }
            let mut previous: Option<&Suggestion> = None;
            for suggestion in &ranked {
                prop_assert!(
                    subsequence_positions(&query, &suggestion.label).is_some(),
                    "{} does not match",
                    suggestion.label
                );
                if let Some(before) = previous {
                    prop_assert!(before.score >= suggestion.score, "score order broke");
                    if before.score == suggestion.score {
                        prop_assert!(before.label <= suggestion.label, "label order broke");
                    }
                }
                previous = Some(suggestion);
            }
            let labels: std::collections::HashSet<&str> =
                ranked.iter().map(|s| s.label.as_str()).collect();
            prop_assert_eq!(labels.len(), ranked.len(), "duplicate labels survive");
        }

        /// rank_paths matches over the path, orders score desc then
        /// depth asc, and dedups by path.
        #[test]
        fn prop_rank_paths_orders_by_score_then_depth(
            query in "[a-zA-Z]{0,4}",
            items in proptest::collection::vec(("[a-z]{1,6}", "[a-z/\\\\]{0,18}"), 0..10),
        ) {
            let items: Vec<PathItem> = items
                .into_iter()
                .map(|(label, path)| PathItem { label, path })
                .collect();
            let ranked = rank_paths(&query, &items);
            for suggestion in &ranked {
                prop_assert!(
                    subsequence_positions(&query, &suggestion.hint).is_some(),
                    "{} does not match",
                    suggestion.hint
                );
            }
            let mut previous: Option<&Suggestion> = None;
            for suggestion in &ranked {
                if let Some(before) = previous {
                    prop_assert!(before.score >= suggestion.score);
                    if before.score == suggestion.score {
                        prop_assert!(
                            depth_of(&before.hint) <= depth_of(&suggestion.hint),
                            "depth order broke"
                        );
                        if depth_of(&before.hint) == depth_of(&suggestion.hint) {
                            prop_assert!(before.label <= suggestion.label);
                        }
                    }
                }
                previous = Some(suggestion);
            }
            let hints: std::collections::HashSet<&str> =
                ranked.iter().map(|s| s.hint.as_str()).collect();
            prop_assert_eq!(hints.len(), ranked.len(), "duplicate paths survive");
        }

        /// The merge interleaves by the documented order and dedups on
        /// the insertion text.
        #[test]
        fn prop_merge_interleaves_by_score_source_and_recency(
            l0 in merge_list_strategy(Source::History),
            l1 in merge_list_strategy(Source::Carapace),
            l2 in merge_list_strategy(Source::Catalog),
            l3 in merge_list_strategy(Source::Files),
        ) {
            let lists = [l0, l1, l2, l3];
            let merged = merge_ranked(lists.clone());
            let got: Vec<(String, Source)> = merged
                .into_iter()
                .map(|candidate| (candidate.insert, candidate.source))
                .collect();
            prop_assert_eq!(got, expected_merge(&lists));
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    #[test]
    fn path_depth_tiebreaks_across_separator_kinds() {
        // Same score (equal length, same unbroken hit run, same
        // word-start bonus) with one separator against two: the shallower
        // path ranks first. The deeper path is listed first so a broken
        // depth cannot win by input order.
        let items = vec![
            PathItem {
                label: "c.rs".into(),
                path: "x/y/c.rs".into(),
            },
            PathItem {
                label: "c.rs".into(),
                path: "abz/c.rs".into(),
            },
        ];
        let ranked = rank_paths("c.rs", &items);
        assert_eq!(ranked[0].hint, "abz/c.rs", "one separator ranks under two");
        assert_eq!(ranked.len(), 2);
    }
}
