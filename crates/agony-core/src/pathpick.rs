//! The pure half of the pitch's drag-and-drop (Part05 5.5.2): given the
//! picked file's absolute path, the target's working directory and the
//! user's choice, produce the text a command line wants — the name, the
//! absolute path, or the path relative to the cwd — quoted when it needs
//! quoting. No new dependency: the relative-path math is component
//! arithmetic written here.
//!
//! Items: `Picked` — the one held-file value (its absolute path);
//! `PickForm` — name / absolute / relative, the paste menu's three
//! choices.
//! Core functions: `pick_text` — the one producer (choice + picked +
//! cwd → insertion text, quoted when it contains a space);
//! `relative_path` — the pathdiff-style core (absolute fallback when no
//! relative path exists, e.g. across Windows drives); `quote_if_needed`
//! — double quotes around anything with a space.
//! Depends on: nothing.
//! Called by: the shell's file panel (grab) and Part06's command bar
//! (drop).
//! Calls: nothing.

/// The one grabbed-file value the shell carries (Part05 5.5.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    /// The grabbed file's absolute path.
    pub path: String,
}

/// The paste menu's three forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickForm {
    /// The file's name alone (`report.md`).
    Name,
    /// The absolute path (`C:\docs\report.md`).
    Absolute,
    /// The path relative to the target's cwd (`..\notes\report.md`).
    Relative,
}

/// The one producer: the insertion text for a choice. Relative falls
/// back to the absolute form when no relative path exists (different
/// Windows drives, mismatched roots).
pub fn pick_text(picked: &str, cwd: &str, form: PickForm) -> String {
    let text = match form {
        PickForm::Name => file_name(picked).to_string(),
        PickForm::Absolute => picked.to_string(),
        PickForm::Relative => relative_path(picked, cwd),
    };
    quote_if_needed(&text)
}

/// The file's name component: everything after the last separator
/// (either kind); the whole string when there is none.
pub fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

/// `picked` relative to `cwd`, component arithmetic (pathdiff-style, no
/// dependency): strip the common prefix, one `..` per remaining cwd
/// component, then the picked remainder. When the roots differ (Windows
/// drive letters, absolute vs relative) there is no relative path — the
/// absolute form comes back instead.
pub fn relative_path(picked: &str, cwd: &str) -> String {
    let (picked_root, picked_rest) = split_root(picked);
    let (cwd_root, cwd_rest) = split_root(cwd);
    if !picked_root.eq_ignore_ascii_case(&cwd_root) {
        return picked.to_string();
    }
    let common = picked_rest
        .iter()
        .zip(cwd_rest.iter())
        .take_while(|(p, c)| p.eq_ignore_ascii_case(c))
        .count();
    let ups = cwd_rest.len() - common;
    if ups == 0 && common == picked_rest.len() {
        return ".".to_string();
    }
    let mut parts: Vec<String> = Vec::with_capacity(ups + picked_rest.len() - common);
    parts.extend(std::iter::repeat_n("..".to_string(), ups));
    parts.extend(picked_rest[common..].iter().cloned());
    parts.join("\\")
}

/// Double-quote a path when it contains a space (both cmd and PowerShell
/// accept that); anything else comes back untouched.
pub fn quote_if_needed(text: &str) -> String {
    if text.contains(' ') && !text.starts_with('"') {
        format!("\"{text}\"")
    } else {
        text.to_string()
    }
}

/// Split a path into its root (drive spec, leading separators, or "" for
/// a relative path) and its remaining components (empty and `.` parts
/// dropped).
fn split_root(path: &str) -> (String, Vec<String>) {
    let mut rest = path;
    let mut root = String::new();
    let drive = rest.as_bytes();
    if drive.len() >= 2 && drive[1] == b':' && drive[0].is_ascii_alphabetic() {
        root = rest[..2].to_string();
        rest = &rest[2..];
    }
    let trimmed = rest.trim_start_matches(['/', '\\']);
    if root.is_empty() && trimmed.len() < rest.len() {
        root = rest[..rest.len() - trimmed.len()].to_string();
    }
    let parts: Vec<String> = trimmed
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_string)
        .collect();
    (root, parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_absolute_relative_forms() {
        let picked = r"C:\Users\me\Documents\my report.md";
        let cwd = r"C:\Users\me";
        assert_eq!(
            pick_text(picked, cwd, PickForm::Name),
            "\"my report.md\"",
            "names with spaces are quoted"
        );
        assert_eq!(
            pick_text(picked, cwd, PickForm::Absolute),
            "\"C:\\Users\\me\\Documents\\my report.md\""
        );
        assert_eq!(
            pick_text(picked, cwd, PickForm::Relative),
            r#""Documents\my report.md""#
        );
    }

    #[test]
    fn plain_paths_stay_unquoted() {
        assert_eq!(
            pick_text(r"C:\docs\report.md", r"C:\docs", PickForm::Relative),
            "report.md"
        );
        assert_eq!(quote_if_needed("simple.txt"), "simple.txt");
    }

    #[test]
    fn relative_path_descends_and_escapes() {
        assert_eq!(
            relative_path(r"C:\a\b\c\file.txt", r"C:\a\b"),
            r"c\file.txt".to_string(),
            "a file in a cwd subdir is a descent"
        );
        assert_eq!(
            relative_path(r"C:\a\file.txt", r"C:\a\b\c"),
            r"..\..\file.txt".to_string()
        );
        assert_eq!(
            relative_path(r"C:\a\x\file.txt", r"C:\a\b\c"),
            r"..\..\x\file.txt".to_string()
        );
    }

    #[test]
    fn same_file_is_a_dot() {
        assert_eq!(relative_path(r"C:\a\b", r"C:\a\b"), ".");
    }

    #[test]
    fn different_drives_fall_back_to_absolute() {
        assert_eq!(
            relative_path(r"D:\data\file.txt", r"C:\Users\me"),
            r"D:\data\file.txt".to_string(),
            "no relative path exists across drives"
        );
        assert_eq!(
            pick_text(r"D:\data\file.txt", r"C:\Users\me", PickForm::Relative),
            r"D:\data\file.txt"
        );
    }

    #[test]
    fn drive_letter_case_and_separator_kinds_do_not_matter() {
        assert_eq!(
            relative_path(r"c:\A\B\file.txt", r"C:\a\c"),
            r"..\B\file.txt".to_string(),
            "one unmatched cwd component is one .."
        );
        assert_eq!(
            relative_path("C:/a/b/file.txt", r"C:\a"),
            "b\\file.txt".to_string()
        );
    }

    #[test]
    fn relative_inputs_fall_back_to_the_absolute_form() {
        assert_eq!(relative_path(r"b\file.txt", r"C:\a"), r"b\file.txt");
        assert_eq!(relative_path(r"C:\a\file.txt", "a"), r"C:\a\file.txt");
    }

    #[test]
    fn file_name_takes_the_last_component_of_either_separator() {
        assert_eq!(file_name(r"C:\a\b\c.txt"), "c.txt");
        assert_eq!(file_name("a/b.txt"), "b.txt");
        assert_eq!(file_name("plain.txt"), "plain.txt");
        assert_eq!(file_name(r"C:\a\b\"), "b");
    }

    #[test]
    fn already_quoted_text_is_not_double_quoted() {
        assert_eq!(quote_if_needed(r#""my file.txt""#), r#""my file.txt""#);
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    fn path_char() -> impl Strategy<Value = char> {
        prop_oneof![
            Just('\\'),
            Just('/'),
            Just(':'),
            Just('.'),
            Just(' '),
            proptest::char::range('a', 'z'),
            proptest::char::range('A', 'Z'),
            proptest::char::range('0', '9'),
        ]
    }

    fn path_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(path_char(), 0..24).prop_map(|chars| chars.into_iter().collect())
    }

    /// Spell a path from canonical parts, flipping case per component by
    /// bits of `mangle` (the matchers are case-insensitive on purpose).
    fn spell_path(root_c: bool, parts: &[String], mangle: u64, shift: u32) -> String {
        let root = match (root_c, mangle & 1 != 0) {
            (true, false) => "c:",
            (true, true) => "C:",
            (false, false) => "d:",
            (false, true) => "D:",
        };
        let mut out = root.to_string();
        for (index, part) in parts.iter().enumerate() {
            let bit = (mangle >> (shift + index as u32 + 1)) & 1 != 0;
            out.push('\\');
            if bit {
                out.push_str(&part.to_uppercase());
            } else {
                out.push_str(part);
            }
        }
        out
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// Quoting is idempotent and quotes exactly the space-bearing,
        /// not-yet-quoted text.
        #[test]
        fn prop_quote_is_idempotent_and_space_driven(text in any::<String>()) {
            let once = quote_if_needed(&text);
            let twice = quote_if_needed(&once);
            prop_assert_eq!(&twice, &once, "quoting is not idempotent");
            if text.contains(' ') && !text.starts_with('"') {
                prop_assert!(once.starts_with('"') && once.ends_with('"'), "{once:?}");
            } else {
                prop_assert_eq!(once, text);
            }
        }

        /// The name component carries no separator and is its own name —
        /// all-separator paths come back unchanged.
        #[test]
        fn prop_file_name_strips_separators(path in path_text()) {
            let name = file_name(&path);
            let bare = path.chars().any(|ch| ch != '/' && ch != '\\');
            if !bare {
                prop_assert_eq!(name, path.as_str());
                return Ok(());
            }
            prop_assert!(!name.contains('/') && !name.contains('\\'), "{name:?}");
            prop_assert!(!name.is_empty());
            prop_assert_eq!(file_name(name), name, "not idempotent");
        }

        /// Relative is pure case-insensitive component arithmetic with
        /// the absolute fallback across roots.
        #[test]
        fn prop_relative_path_is_component_arithmetic(
            picked_root in "[cd]",
            cwd_root in "[cd]",
            picked_parts in proptest::collection::vec("[a-z]{1,4}", 1..6),
            cwd_parts in proptest::collection::vec("[a-z]{1,4}", 0..6),
            mangle in any::<u64>(),
        ) {
            let picked = spell_path(picked_root == "c", &picked_parts, mangle, 0);
            let cwd = spell_path(cwd_root == "c", &cwd_parts, mangle, 16);
            let result = relative_path(&picked, &cwd);
            if picked_root != cwd_root {
                prop_assert_eq!(result, picked, "cross-root must fall back");
                return Ok(());
            }
            let common = picked_parts
                .iter()
                .zip(cwd_parts.iter())
                .take_while(|(picked, cwd)| picked.eq_ignore_ascii_case(cwd))
                .count();
            let ups = cwd_parts.len() - common;
            // The remainder keeps the picked path's own spelling.
            let spelled: Vec<String> = (0..picked_parts.len())
                .map(|index| (mangle >> (index + 1)) & 1 != 0)
                .zip(picked_parts.iter())
                .map(|(up, part)| if up { part.to_uppercase() } else { part.clone() })
                .collect();
            let expected = if ups == 0 && common == picked_parts.len() {
                ".".to_string()
            } else {
                let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), ups).collect();
                parts.extend(spelled[common..].iter().cloned());
                parts.join("\\")
            };
            prop_assert_eq!(result, expected, "picked {} from cwd {}", picked, cwd);
        }

        /// The paste menu's three forms are the three producers, quoted.
        #[test]
        fn prop_pick_text_forms_follow_the_producers(
            picked in path_text(),
            cwd in path_text(),
            form_pick in 0u8..3,
        ) {
            let form = match form_pick {
                0 => PickForm::Name,
                1 => PickForm::Absolute,
                _ => PickForm::Relative,
            };
            let expected = match form {
                PickForm::Name => quote_if_needed(file_name(&picked)),
                PickForm::Absolute => quote_if_needed(&picked),
                PickForm::Relative => quote_if_needed(&relative_path(&picked, &cwd)),
            };
            prop_assert_eq!(pick_text(&picked, &cwd, form), expected);
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    #[test]
    fn leading_separator_roots_do_not_match_drive_roots() {
        assert_eq!(
            relative_path(r"\docs\f.txt", r"C:\a"),
            r"\docs\f.txt",
            "separator-rooted picked from a drive cwd"
        );
        assert_eq!(
            relative_path(r"C:\a\f.txt", r"\docs"),
            r"C:\a\f.txt",
            "drive-rooted picked from a separator cwd"
        );
        assert_eq!(
            relative_path(r"\docs\f.txt", r"\docs"),
            "f.txt",
            "the same separator root descends"
        );
        assert_eq!(
            relative_path(r"\docs", r"\docs"),
            ".",
            "identical separator roots are a dot"
        );
        assert_eq!(
            relative_path(r"\docs\f.txt", r"x\y"),
            r"\docs\f.txt",
            "a separator root is not a relative root"
        );
    }
}
