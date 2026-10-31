//! The one truncation helper: cut-to-width with an ellipsis, both ends.
//!
//! Items: (functions only).
//! Core functions: `ellipsize` — cut a string to `budget` cells, `…`
//! marking a right cut; `ellipsize_left` — keep the tail (a path's last
//! cells), `…` marking a left cut.
//! Depends on: nothing.
//! Called by: the shell's draw modules (wm, taskbar, commandbar,
//! startmenu) — the one implementation the reuse rule asks for.
//! Calls: nothing.

/// Cut `text` to at most `budget` characters, `…` marking the cut.
/// Shorter texts pass through untouched; a zero budget is empty.
pub fn ellipsize(text: &str, budget: usize) -> String {
    let count = text.chars().count();
    if count <= budget {
        return text.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    let mut shown: String = text.chars().take(budget - 1).collect();
    shown.push('…');
    shown
}

/// Keep the last `budget` characters of `text`, `…` marking the cut
/// (the command bar's context path keeps its tail).
pub fn ellipsize_left(text: &str, budget: usize) -> String {
    let count = text.chars().count();
    if count <= budget {
        return text.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    let mut shown = String::from("…");
    shown.extend(text.chars().skip(count - budget + 1));
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_passes_through() {
        assert_eq!(ellipsize("abc", 5), "abc");
        assert_eq!(ellipsize("abc", 3), "abc");
        assert_eq!(ellipsize_left(r"C:\bin", 10), r"C:\bin");
    }

    #[test]
    fn a_long_text_is_cut_with_the_marker() {
        let cut = ellipsize("abcdefgh", 5);
        assert_eq!(cut, "abcd…");
        assert_eq!(cut.chars().count(), 5);
        let left = ellipsize_left("abcdefgh", 5);
        assert_eq!(left, "…efgh");
        assert_eq!(left.chars().count(), 5);
    }

    #[test]
    fn a_zero_budget_is_empty_never_panics() {
        assert_eq!(ellipsize("abc", 0), "");
        assert_eq!(ellipsize_left("abc", 0), "");
    }

    #[test]
    fn counts_characters_not_bytes() {
        let cut = ellipsize("aäääb", 3);
        assert_eq!(cut.chars().count(), 3, "characters, never bytes");
    }
}
