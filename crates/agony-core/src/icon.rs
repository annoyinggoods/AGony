//! The single glyph table: every UI icon as a Nerd Font codepoint with an
//! ASCII fallback.
//!
//! Items: `Icon` — the icons the UI needs (also the names app manifests
//! may reference); `ALL_ICONS` — every variant, one place (the tests
//! iterate it, `from_name` looks up in it).
//! Core functions: `glyph` — the one lookup; `name` / `from_name` — the
//! manifest-facing spelling of each variant.
//! No module writes a glyph literal: they all go through `glyph`, which
//! decides by `FontMode` (Auto resolves to ASCII in the shell before it
//! reaches this table).
//! Depends on: crate::config (FontMode).
//! Called by: the shell's taskbar, start menu and doctor; manifests
//! validate their `icon` field through `from_name`.
//! Calls: nothing outside this crate.

use crate::config::FontMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Start,
    Search,
    Folder,
    App,
    Book,
    Tv,
    Film,
    Gamepad,
    Globe,
    ChevronRight,
    Power,
}

/// The one glyph lookup: Nerd Font codepoint or ASCII fallback per mode.
pub fn glyph(icon: Icon, mode: FontMode) -> &'static str {
    if mode == FontMode::Nerd {
        nerd(icon)
    } else {
        ascii(icon)
    }
}

/// The manifest-facing spelling of a variant (lowercase, snake-free).
pub fn name(icon: Icon) -> &'static str {
    match icon {
        Icon::Start => "start",
        Icon::Search => "search",
        Icon::Folder => "folder",
        Icon::App => "app",
        Icon::Book => "book",
        Icon::Tv => "tv",
        Icon::Film => "film",
        Icon::Gamepad => "gamepad",
        Icon::Globe => "globe",
        Icon::ChevronRight => "chevron_right",
        Icon::Power => "power",
    }
}

/// The variant behind a manifest's `icon` field, or None when unknown.
pub fn from_name(name: &str) -> Option<Icon> {
    ALL_ICONS
        .iter()
        .copied()
        .find(|icon| self::name(*icon) == name)
}

/// The two-letter tile identity (Part09 9.4.2): the name's first two
/// alphanumeric characters, uppercased, Unicode letters kept — letters,
/// never glyph codepoints, so a tile reads on any font (the desktop's
/// ASCII-safety rule). Missing characters pad with `_`.
pub fn initials(name: &str) -> [char; 2] {
    let picked: Vec<char> = name
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .take(2)
        .collect();
    let at = |index: usize| {
        picked
            .get(index)
            .map(|ch| ch.to_uppercase().next().unwrap_or(*ch))
            .unwrap_or('_')
    };
    [at(0), at(1)]
}

fn nerd(icon: Icon) -> &'static str {
    match icon {
        Icon::Start => "\u{f17b}",        // nf-fa-windows
        Icon::Search => "\u{f002}",       // nf-fa-search
        Icon::Folder => "\u{f07b}",       // nf-fa-folder
        Icon::App => "\u{f120}",          // nf-fa-terminal
        Icon::Book => "\u{f02d}",         // nf-fa-book
        Icon::Tv => "\u{f26c}",           // nf-fa-tv
        Icon::Film => "\u{f008}",         // nf-fa-film
        Icon::Gamepad => "\u{f11b}",      // nf-fa-gamepad
        Icon::Globe => "\u{f0ac}",        // nf-fa-globe
        Icon::ChevronRight => "\u{f054}", // nf-fa-chevron-right
        Icon::Power => "\u{f011}",        // nf-fa-power-off
    }
}

fn ascii(icon: Icon) -> &'static str {
    match icon {
        Icon::Start => "Start",
        Icon::Search => "find",
        Icon::Folder => "dir",
        Icon::App => "run",
        Icon::Book => "read",
        Icon::Tv => "view",
        Icon::Film => "gif",
        Icon::Gamepad => "play",
        Icon::Globe => "net",
        Icon::ChevronRight => ">",
        Icon::Power => "exit",
    }
}

/// Every icon, in one place: the tests iterate it, `from_name` looks up in it.
pub const ALL_ICONS: [Icon; 11] = [
    Icon::Start,
    Icon::Search,
    Icon::Folder,
    Icon::App,
    Icon::Book,
    Icon::Tv,
    Icon::Film,
    Icon::Gamepad,
    Icon::Globe,
    Icon::ChevronRight,
    Icon::Power,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_both_representations() {
        for icon in ALL_ICONS {
            assert!(!nerd(icon).is_empty(), "{icon:?} has no nerd glyph");
            assert!(!ascii(icon).is_empty(), "{icon:?} has no ascii fallback");
        }
    }

    #[test]
    fn every_nerd_glyph_is_a_nerd_font_codepoint() {
        for icon in ALL_ICONS {
            assert!(
                !nerd(icon).is_ascii(),
                "{icon:?} nerd form should not be plain ASCII"
            );
        }
    }

    #[test]
    fn invariant_ascii_mode_emits_no_nerd_codepoints() {
        let mut all = String::new();
        for icon in ALL_ICONS {
            all.push_str(glyph(icon, FontMode::Ascii));
        }
        assert!(all.is_ascii(), "ascii mode leaked: {all:?}");
    }

    #[test]
    fn auto_mode_falls_back_to_ascii() {
        // The shell resolves auto -> ascii before asking; the table agrees.
        for icon in ALL_ICONS {
            assert_eq!(glyph(icon, FontMode::Auto), ascii(icon));
        }
    }

    #[test]
    fn names_round_trip_through_from_name() {
        for icon in ALL_ICONS {
            assert_eq!(from_name(name(icon)), Some(icon), "{icon:?} name broken");
        }
        assert_eq!(from_name("nope"), None);
        assert_eq!(from_name("App"), None, "names are lowercase only");
    }

    // ------------------------------------------------- initials (Part09)

    #[test]
    fn initials_take_the_first_two_alphanumerics_uppercased() {
        assert_eq!(initials("Superfile"), ['S', 'U']);
        assert_eq!(initials("git"), ['G', 'I']);
        assert_eq!(initials("my-tool"), ['M', 'Y']);
        assert_eq!(initials("3D viewer"), ['3', 'D'], "digits count");
        assert_eq!(initials("  spaced  out "), ['S', 'P']);
    }

    #[test]
    fn initials_keep_unicode_letters_and_pad_short_names() {
        assert_eq!(initials("Ünix tool"), ['Ü', 'N'], "Unicode letters stay");
        assert_eq!(initials("Æther"), ['Æ', 'T']);
        assert_eq!(initials("C:"), ['C', '_'], "one letter pads");
        assert_eq!(initials(""), ['_', '_'], "empty pads fully");
        assert_eq!(initials("..."), ['_', '_'], "punctuation is not a letter");
    }

    #[test]
    fn initials_never_produce_non_letter_glyphs() {
        // The desktop's rule: letters render everywhere; there is no
        // Nerd Font codepoint in a tile by construction.
        for name in ["glow", "Ünix", "3D", ""] {
            for ch in initials(name) {
                assert!(ch.is_alphanumeric() || ch == '_', "{name}: {ch:?} leaked");
            }
        }
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    /// The documented initials rule, written out independently.
    fn expected_initials(name: &str) -> [char; 2] {
        let picked: Vec<char> = name
            .chars()
            .filter(|ch| ch.is_alphanumeric())
            .take(2)
            .collect();
        let at = |index: usize| {
            picked
                .get(index)
                .map(|ch| ch.to_uppercase().next().unwrap_or(*ch))
                .unwrap_or('_')
        };
        [at(0), at(1)]
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// initials are the first two alphanumerics, uppercased, padded —
        /// and always letterlike (the tile's ASCII-safety rule), for any
        /// name at all.
        #[test]
        fn invariant_initials_letterlike_for_any_name(name in any::<String>()) {
            let got = initials(&name);
            prop_assert_eq!(got, expected_initials(&name), "initials of {:?}", name);
            for ch in got {
                prop_assert!(
                    ch.is_alphanumeric() || ch == '_',
                    "{:?}: {:?} leaked",
                    name,
                    ch
                );
            }
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    #[test]
    fn the_ascii_fallbacks_are_part_of_the_ui_contract() {
        assert_eq!(glyph(Icon::Start, FontMode::Ascii), "Start");
        assert_eq!(glyph(Icon::Search, FontMode::Ascii), "find");
        assert_eq!(glyph(Icon::ChevronRight, FontMode::Ascii), ">");
        assert_eq!(glyph(Icon::Power, FontMode::Ascii), "exit");
    }
}
