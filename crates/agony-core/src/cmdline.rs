//! The command bar's line model (Part07 7.2.3): the raw text, the
//! cursor, and the words derived from both — with quoting-aware
//! splitting. One tokenizer serves execution (whole-line word values)
//! and completion (the active word and the text before it) alike.
//!
//! Items: `Line` — the raw string plus a char cursor and the editing
//! moves; `Word` — one raw word with its char range; `Parsed` — the
//! derived view (`words`, `active`, `active_text`).
//! Core functions: `parse` — split a line at a cursor (the one
//! tokenizer); `Line::insert` / `backspace` / `delete` / `left` /
//! `right` / `home` / `end` — the edits; `values_of` — a line's
//! unquoted word values (execution's argv); `replace_active` — swap the
//! active word for a completion, cursor landing after it;
//! `unquote` — strip one level of double quotes.
//! Depends on: nothing.
//! Called by: the shell's `commandbar` (editing, completion, execution).
//! Calls: nothing.

/// One command line: the text plus the cursor as a char offset
/// (0 ≤ cursor ≤ chars). Nothing here interprets the words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub cursor: usize,
}

/// One raw word: its text exactly as typed (quotes included) and its
/// char range in the line (`[start, end)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// The derived view `parse` produces. `active` indexes `words` (or
/// equals `words.len()` when the cursor opens a fresh word);
/// `active_text` is the active word's raw text ("" for a fresh word).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub words: Vec<Word>,
    pub active: usize,
    pub active_text: String,
}

impl Line {
    pub fn new(text: impl Into<String>) -> Line {
        let text = text.into();
        let cursor = text.chars().count();
        Line { text, cursor }
    }

    /// Insert one character at the cursor.
    pub fn insert(&mut self, ch: char) {
        self.insert_str(&ch.to_string());
    }

    /// Insert text at the cursor (the paste menu's path).
    pub fn insert_str(&mut self, text: &str) {
        let at = self.char_offset(self.cursor);
        self.text.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    /// Remove the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.char_offset(self.cursor);
        let start = self.char_offset(self.cursor - 1);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    /// Remove the character under the cursor.
    pub fn delete(&mut self) {
        if self.cursor >= self.text.chars().count() {
            return;
        }
        self.right();
        self.backspace();
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// The byte offset of the `n`th char (the string edits work in
    /// bytes; the cursor and the tokenizer work in chars).
    fn char_offset(&self, n: usize) -> usize {
        char_byte(&self.text, n)
    }
}

/// The byte offset of the `n`th char of `text` (the end when `n` is
/// past it).
fn char_byte(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map(|(at, _)| at)
        .unwrap_or(text.len())
}

/// The one tokenizer: split `text` into raw words on unquoted spaces,
/// then find the word under `cursor` (a cursor at a word's `end`
/// counts as inside it — completing "git che␣" completes "che"; a
/// cursor after a trailing space opens a fresh word).
pub fn parse(text: &str, cursor: usize) -> Parsed {
    let words = split_words(text);
    let active = words
        .iter()
        .position(|word| cursor >= word.start && cursor <= word.end)
        .unwrap_or(words.len());
    let active_text = words
        .get(active)
        .map(|word| word.text.clone())
        .unwrap_or_default();
    Parsed {
        words,
        active,
        active_text,
    }
}

/// The line's unquoted word values — execution's argv shape. Quoted
/// segments contribute their inner text as one word.
pub fn values_of(text: &str) -> Vec<String> {
    split_words(text)
        .iter()
        .map(|word| unquote(&word.text))
        .collect()
}

/// Everything before the active word, as unquoted values (the carapace
/// context: `git che` → `["git"]`).
pub fn prefix_values(text: &str, cursor: usize) -> Vec<String> {
    let parsed = parse(text, cursor);
    parsed
        .words
        .iter()
        .take(parsed.active)
        .map(|word| unquote(&word.text))
        .collect()
}

/// Swap the active word (found again by the same tokenizer) for
/// `replacement` raw text; the returned cursor lands just after it.
/// Quoting is the caller's business (the candidate carries its final
/// text). No-op when the line's words no longer parse.
pub fn replace_active(text: &str, cursor: usize, replacement: &str) -> (String, usize) {
    let parsed = parse(text, cursor);
    match parsed.words.get(parsed.active) {
        Some(word) => {
            let start = char_byte(text, word.start);
            let end = char_byte(text, word.end);
            let mut out = String::with_capacity(text.len() + replacement.len());
            out.push_str(&text[..start]);
            out.push_str(replacement);
            out.push_str(&text[end..]);
            let new_cursor = word.start + replacement.chars().count();
            (out, new_cursor)
        }
        // A fresh word: splice at the cursor's char offset.
        None => {
            let at = char_byte(text, cursor);
            let mut out = String::with_capacity(text.len() + replacement.len());
            out.push_str(&text[..at]);
            out.push_str(replacement);
            let new_cursor = cursor + replacement.chars().count();
            (out, new_cursor)
        }
    }
}

/// Strip one level of double quotes (the shell's word value).
pub fn unquote(word: &str) -> String {
    let trimmed = word.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        return trimmed[1..trimmed.len() - 1].to_string();
    }
    trimmed.to_string()
}

fn split_words(text: &str) -> Vec<Word> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut current: Option<Word> = None;
    let mut in_quotes = false;
    for (index, &ch) in chars.iter().enumerate() {
        if ch == '"' {
            in_quotes = !in_quotes;
            touch(&mut current, index);
        } else if ch.is_whitespace() && !in_quotes {
            if let Some(mut word) = current.take() {
                word.end = index;
                words.push(word);
            }
        } else {
            touch(&mut current, index);
        }
    }
    if let Some(mut word) = current.take() {
        word.end = chars.len();
        words.push(word);
    }
    for word in &mut words {
        word.text = chars[word.start..word.end].iter().collect();
    }
    words
}

/// Open the word under construction when none is open, recording its
/// start (the text is filled in from the range once splitting ends).
fn touch(current: &mut Option<Word>, index: usize) {
    if current.is_none() {
        *current = Some(Word {
            text: String::new(),
            start: index,
            end: index,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_of(text: &str, cursor: usize) -> Parsed {
        parse(text, cursor)
    }

    #[test]
    fn words_split_on_unquoted_spaces() {
        let parsed = parsed_of("git checkout -b main", 20);
        let texts: Vec<&str> = parsed.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["git", "checkout", "-b", "main"]);
        assert_eq!(parsed.active, 3);
        assert_eq!(parsed.active_text, "main");
    }

    #[test]
    fn quotes_group_words() {
        let parsed = parsed_of(r#"echo "two words""#, 16);
        let texts: Vec<&str> = parsed.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["echo", r#""two words""#]);
        assert_eq!(values_of(r#"echo "two words""#), ["echo", "two words"]);
    }

    #[test]
    fn quoted_space_stays_inside_the_word() {
        let parsed = parsed_of(r#"open "my file.txt""#, 18);
        assert_eq!(parsed.words.len(), 2);
        assert_eq!(parsed.active_text, r#""my file.txt""#);
    }

    #[test]
    fn mid_word_cursor_selects_that_word() {
        let parsed = parsed_of("git checkout", 2);
        assert_eq!(parsed.active, 0, "cursor inside `git`");
        assert_eq!(parsed.active_text, "git");
        let parsed = parsed_of("git checkout", 4);
        assert_eq!(parsed.active, 1, "cursor at a word's start is inside it");
    }

    #[test]
    fn cursor_at_word_end_completes_that_word() {
        let parsed = parsed_of("git che", 7);
        assert_eq!(parsed.active, 1);
        assert_eq!(parsed.active_text, "che");
    }

    #[test]
    fn trailing_space_opens_a_fresh_word() {
        let parsed = parsed_of("git checkout ", 13);
        assert_eq!(parsed.active, 2, "beyond the last word");
        assert_eq!(parsed.active_text, "");
    }

    #[test]
    fn empty_line_parses_to_a_fresh_word() {
        let parsed = parsed_of("", 0);
        assert!(parsed.words.is_empty());
        assert_eq!(parsed.active, 0);
        assert_eq!(parsed.active_text, "");
    }

    #[test]
    fn prefix_values_stop_before_the_active_word() {
        assert_eq!(prefix_values("git che", 7), ["git"]);
        assert_eq!(prefix_values("git checkout ", 13), ["git", "checkout"]);
        assert!(prefix_values("che", 3).is_empty());
    }

    #[test]
    fn editing_moves_the_cursor_and_the_text() {
        let mut line = Line::new("ab");
        line.left();
        line.insert('c');
        assert_eq!(line.text, "acb");
        assert_eq!(line.cursor, 2);
        line.backspace();
        assert_eq!(line.text, "ab");
        assert_eq!(line.cursor, 1);
        line.delete();
        assert_eq!(line.text, "a");
        assert_eq!(line.cursor, 1, "delete at the end is a no-op");
        line.home();
        assert_eq!(line.cursor, 0);
        line.end();
        assert_eq!(line.cursor, 1);
    }

    #[test]
    fn backspace_at_home_and_left_clamp_at_zero() {
        let mut line = Line::default();
        line.backspace();
        line.left();
        assert_eq!(line.cursor, 0);
        assert_eq!(line.text, "");
    }

    #[test]
    fn insert_str_lands_at_the_cursor() {
        let mut line = Line::new("git ");
        line.insert_str(r#""my file.txt""#);
        assert_eq!(line.text, r#"git "my file.txt""#);
        assert_eq!(line.cursor, line.text.chars().count());
    }

    #[test]
    fn replace_active_swaps_the_word_under_construction() {
        let (text, cursor) = replace_active("git che", 7, "checkout");
        assert_eq!(text, "git checkout");
        assert_eq!(cursor, text.chars().count(), "the cursor lands after");
        let parsed = parse(&text, cursor);
        assert_eq!(parsed.active, 1);
        assert_eq!(parsed.active_text, "checkout");
    }

    #[test]
    fn replace_active_mid_line_keeps_the_rest() {
        let (text, cursor) = replace_active("git che -b main", 7, "checkout");
        assert_eq!(text, "git checkout -b main");
        assert_eq!(cursor, "git checkout".chars().count());
    }

    #[test]
    fn replace_active_on_a_fresh_word_appends() {
        let (text, cursor) = replace_active("git ", 4, "checkout ");
        assert_eq!(text, "git checkout ");
        assert_eq!(cursor, text.chars().count());
    }

    #[test]
    fn replace_active_quotes_survive_as_raw_text() {
        let (text, _) = replace_active(r#"open "my"#, 8, "all");
        assert_eq!(text, "open all");
    }

    #[test]
    fn unquote_strips_one_level() {
        assert_eq!(unquote(r#""two words""#), "two words");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(unquote("\""), "\"");
        assert_eq!(unquote(" spaced "), "spaced");
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// Word ranges never overlap, stay in bounds and re-spell their
        /// text — for any input line.
        #[test]
        fn prop_word_ranges_are_bounded_sorted_and_textual(text in any::<String>()) {
            let chars: Vec<char> = text.chars().collect();
            let mut previous_end = 0usize;
            for word in &parse(&text, 0).words {
                prop_assert!(word.start < word.end, "empty word {word:?}");
                prop_assert!(word.start >= previous_end, "words overlap at {}", word.start);
                prop_assert!(word.end <= chars.len(), "range past the end");
                let spelled: String = chars[word.start..word.end].iter().collect();
                prop_assert_eq!(&word.text, &spelled);
                previous_end = word.end;
            }
        }

        /// The active word is exactly the word under the cursor; a fresh
        /// word only when no word is under it.
        #[test]
        fn prop_active_is_the_word_under_the_cursor(
            text in any::<String>(),
            raw in any::<usize>(),
        ) {
            let cursor = raw % (text.chars().count() + 1);
            let parsed = parse(&text, cursor);
            prop_assert!(parsed.active <= parsed.words.len());
            match parsed.words.get(parsed.active) {
                Some(word) => prop_assert!(
                    cursor >= word.start && cursor <= word.end,
                    "active {} not under cursor {}",
                    parsed.active,
                    cursor
                ),
                None => {
                    let under = parsed
                        .words
                        .iter()
                        .any(|word| cursor >= word.start && cursor <= word.end);
                    prop_assert!(!under, "a fresh word while one sits under the cursor");
                }
            }
        }

        /// Any edit sequence keeps the cursor inside the text.
        #[test]
        fn prop_edit_sequences_keep_the_cursor_inside(
            text in any::<String>(),
            edits in proptest::collection::vec((0u8..7, any::<char>()), 0..40),
        ) {
            let mut line = Line::new(text);
            for (kind, ch) in edits {
                match kind {
                    0 => line.insert(ch),
                    1 => line.backspace(),
                    2 => line.delete(),
                    3 => line.left(),
                    4 => line.right(),
                    5 => line.home(),
                    _ => line.end(),
                }
                prop_assert!(
                    line.cursor <= line.text.chars().count(),
                    "cursor escaped the text"
                );
            }
        }

        /// insert then backspace restores the exact line, at any cursor.
        #[test]
        fn prop_backspace_inverts_insert(
            text in any::<String>(),
            raw in any::<usize>(),
            ch in any::<char>(),
        ) {
            let mut line = Line {
                text: text.clone(),
                cursor: raw % (text.chars().count() + 1),
            };
            let before = line.clone();
            line.insert(ch);
            line.backspace();
            prop_assert_eq!(line, before, "insert+backspace is not identity");
        }

        /// Replacing the active word with its own raw text is a no-op.
        #[test]
        fn prop_replace_active_with_the_same_word_keeps_the_text(
            text in any::<String>(),
            raw in any::<usize>(),
        ) {
            let cursor = raw % (text.chars().count() + 1);
            let parsed = parse(&text, cursor);
            let Some(word) = parsed.words.get(parsed.active) else {
                return Ok(());
            };
            let (out, out_cursor) = replace_active(&text, cursor, &word.text);
            prop_assert_eq!(&out, &text);
            prop_assert!(out_cursor <= out.chars().count());
        }

        /// values_of is the parse pipeline over every word.
        #[test]
        fn prop_values_of_unquotes_every_word(text in any::<String>()) {
            let expected: Vec<String> = parse(&text, 0)
                .words
                .iter()
                .map(|word| unquote(&word.text))
                .collect();
            prop_assert_eq!(values_of(&text), expected);
        }
    }
}
