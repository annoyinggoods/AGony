//! The neutral key table: a platform-free key model, the key names the
//! multiplexer expects, and the reserved-key combos read from settings.
//!
//! Items: `Key` — a key without modifiers; `Mods` — modifier flags;
//! `Combo` — key + modifiers; `Action` — what to do with a press, as pure
//! data (`Type` text, `Press` a named key, `Ignore`).
//! Core functions: `action_for` — the one mapping table; `combo_from_str`
//! — parse a settings combo like "ctrl-q" or "minus";
//! `combo_to_str` — the canonical (lowercase-char) spelling back;
//! `DEFAULT_QUIT` / `DEFAULT_START_MENU` — the reserved-key defaults.
//! Depends on: nothing.
//! Called by: the shell's `input` module (forwarding) and `app` (reserved
//! keys); Part04's focus model reuses the same table; Part07's command
//! bar owns its two modal keys (`DEFAULT_BAR` is reserved,
//! `BAR_AUTOFILL`/`BAR_PASTE` are bar-local).
//! Calls: nothing.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Escape,
    Tab,
    BackTab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combo {
    pub key: Key,
    pub mods: Mods,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Type(String),
    Press(String),
    Ignore,
}

pub const DEFAULT_QUIT: &str = "ctrl-q";
pub const DEFAULT_START_MENU: &str = "ctrl-space";
/// The window keys (Part04 4.5.1): cycle focus, close, minimize,
/// maximize — all overridable from settings, documented in the taskbar's
/// hint line. Cycle focus is `ctrl-e`, not the desktop-conventional
/// `ctrl-tab`: hosting terminals (Windows Terminal, VS Code) consume
/// that chord for their own tab switching before AGony ever sees it, and
/// a default that dies on the primary platform is no default. `e` sits
/// beside `w`/`q` next to Tab, keeping the cluster.
pub const DEFAULT_CYCLE_FOCUS: &str = "ctrl-e";
pub const DEFAULT_CLOSE_WINDOW: &str = "ctrl-w";
pub const DEFAULT_MINIMIZE: &str = "ctrl-m";
pub const DEFAULT_MAXIMIZE: &str = "ctrl-f";
/// The command bar's reserved open/close key (Part07 7.2.1).
pub const DEFAULT_BAR: &str = "ctrl-k";
/// Bar-local keys (Part07): active only while the bar is open — the bar
/// consumes keys before the reserved router sees them, so these reuse
/// combos that mean something else globally (ctrl-f maximizes; ctrl-g
/// is unassigned). Deliberately NOT in `DEFAULTS`: the collision
/// invariant guards global reserved keys, and a modal surface cannot
/// make one ambiguous.
pub const BAR_AUTOFILL: &str = "ctrl-f";
pub const BAR_PASTE: &str = "ctrl-g";

/// Every reserved default, one place: collision detection and tests
/// iterate it.
pub const DEFAULTS: [&str; 7] = [
    DEFAULT_QUIT,
    DEFAULT_START_MENU,
    DEFAULT_CYCLE_FOCUS,
    DEFAULT_CLOSE_WINDOW,
    DEFAULT_MINIMIZE,
    DEFAULT_MAXIMIZE,
    DEFAULT_BAR,
];

/// The quit default as a value — the last resort for callers that must
/// return a `Combo` even when re-parsing the default spec fails (pinned
/// impossible by `invariant_reserved_defaults_do_not_collide`). The literal
/// lives here, next to its string form, and nowhere else.
pub fn default_quit_combo() -> Combo {
    Combo {
        key: Key::Char('q'),
        mods: Mods {
            ctrl: true,
            ..Mods::default()
        },
    }
}

/// The one mapping table: a key press to the pure action the shell should
/// perform. Ctrl combos become named presses (`C-<ch>`); Alt combos are not
/// forwarded (spike semantics); plain keys are typed or pressed by name.
pub fn action_for(combo: &Combo) -> Action {
    if combo.mods.ctrl {
        return ctrl_action(&combo.key);
    }
    if combo.mods.alt {
        return Action::Ignore;
    }
    plain_action(&combo.key)
}

/// Parse a settings combo spec: "ctrl-q", "ctrl-space", "shift-tab",
/// "minus", "q". Unknown modifiers, unknown key names and multi-char
/// keys all return None. Char keys parse lowercase — the canonical form
/// the reserved-key matcher compares against.
pub fn combo_from_str(spec: &str) -> Option<Combo> {
    let lowered = spec.to_ascii_lowercase();
    let parts: Vec<&str> = lowered.split('-').collect();
    let (key_part, prefixes) = parts.split_last()?;
    let mut mods = Mods::default();
    if !prefixes.iter().all(|p| apply_modifier(&mut mods, p)) {
        return None;
    }
    Some(Combo {
        key: key_from_str(key_part)?,
        mods,
    })
}

fn ctrl_action(key: &Key) -> Action {
    let Key::Char(ch) = key else {
        return Action::Ignore;
    };
    Action::Press(format!("C-{}", ch.to_ascii_lowercase()))
}

fn plain_action(key: &Key) -> Action {
    match key {
        Key::Char(ch) => Action::Type(ch.to_string()),
        Key::Enter => press("Enter"),
        Key::Backspace => press("BSpace"),
        Key::Delete => press("Delete"),
        Key::Escape => press("Escape"),
        Key::Tab => press("Tab"),
        Key::BackTab => press("BTab"),
        Key::Left => press("Left"),
        Key::Right => press("Right"),
        Key::Up => press("Up"),
        Key::Down => press("Down"),
        Key::Home => press("Home"),
        Key::End => press("End"),
        Key::PageUp => press("PageUp"),
        Key::PageDown => press("PageDown"),
    }
}

fn press(name: &str) -> Action {
    Action::Press(name.to_string())
}

fn apply_modifier(mods: &mut Mods, token: &str) -> bool {
    match token {
        "ctrl" => mods.ctrl = true,
        "alt" => mods.alt = true,
        "shift" => mods.shift = true,
        _ => return false,
    }
    true
}

fn key_from_str(token: &str) -> Option<Key> {
    let named = match token {
        "space" => Some(Key::Char(' ')),
        "minus" => Some(Key::Char('-')),
        "enter" => Some(Key::Enter),
        "esc" | "escape" => Some(Key::Escape),
        "tab" => Some(Key::Tab),
        "btab" | "backtab" => Some(Key::BackTab),
        "bspace" | "backspace" => Some(Key::Backspace),
        "delete" | "del" => Some(Key::Delete),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        _ => None,
    };
    named.or_else(|| single_char(token))
}

fn single_char(token: &str) -> Option<Key> {
    let ch = token.chars().next()?;
    (token.chars().count() == 1).then_some(Key::Char(ch))
}

/// The settings spelling of a combo ("ctrl-q", "ctrl-space", "minus")
/// — the inverse of [`combo_from_str`] on the *canonical* form: char
/// keys spell lowercase (`Q` as `q`, `-` as `minus`), which is what
/// matching compares (`input::matches_combo` normalizes both sides).
/// The settings window's pressed-to-set key editor writes it.
pub fn combo_to_str(combo: Combo) -> String {
    let mut out = String::new();
    let Mods { ctrl, alt, shift } = combo.mods;
    for (on, name) in [(ctrl, "ctrl"), (alt, "alt"), (shift, "shift")] {
        if on {
            out.push_str(name);
            out.push('-');
        }
    }
    out.push_str(&key_to_str(combo.key));
    out
}

fn key_to_str(key: Key) -> String {
    match key {
        Key::Char(' ') => "space".into(),
        Key::Char('-') => "minus".into(),
        Key::Char(ch) => ch.to_ascii_lowercase().to_string(),
        Key::Enter => "enter".into(),
        Key::Backspace => "backspace".into(),
        Key::Delete => "delete".into(),
        Key::Escape => "esc".into(),
        Key::Tab => "tab".into(),
        Key::BackTab => "backtab".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KEYS: [Key; 15] = [
        Key::Char('a'),
        Key::Enter,
        Key::Backspace,
        Key::Delete,
        Key::Escape,
        Key::Tab,
        Key::BackTab,
        Key::Left,
        Key::Right,
        Key::Up,
        Key::Down,
        Key::Home,
        Key::End,
        Key::PageUp,
        Key::PageDown,
    ];

    #[test]
    fn every_plain_key_maps_to_a_nonempty_action() {
        for key in ALL_KEYS {
            let action = action_for(&Combo {
                key,
                mods: Mods::default(),
            });
            let described = match action {
                Action::Type(text) => text,
                Action::Press(name) => name,
                Action::Ignore => String::new(),
            };
            assert!(!described.is_empty(), "{key:?} mapped to nothing");
        }
    }

    #[test]
    fn named_presses_use_the_multiplexers_names() {
        let enter = action_for(&plain(Key::Enter));
        assert_eq!(enter, Action::Press("Enter".into()));
        let backspace = action_for(&plain(Key::Backspace));
        assert_eq!(backspace, Action::Press("BSpace".into()));
        let left = action_for(&plain(Key::Left));
        assert_eq!(left, Action::Press("Left".into()));
    }

    #[test]
    fn ctrl_chars_become_named_presses() {
        let action = action_for(&Combo {
            key: Key::Char('q'),
            mods: Mods {
                ctrl: true,
                ..Mods::default()
            },
        });
        assert_eq!(action, Action::Press("C-q".into()));
    }

    #[test]
    fn alt_combos_are_not_forwarded() {
        let action = action_for(&Combo {
            key: Key::Char('x'),
            mods: Mods {
                alt: true,
                ..Mods::default()
            },
        });
        assert_eq!(action, Action::Ignore);
    }

    #[test]
    fn combo_parsing_covers_the_reserved_defaults() {
        let quit = combo_from_str(DEFAULT_QUIT);
        let start = combo_from_str(DEFAULT_START_MENU);
        assert_eq!(
            quit,
            Some(Combo {
                key: Key::Char('q'),
                mods: Mods {
                    ctrl: true,
                    ..Mods::default()
                }
            })
        );
        assert_eq!(
            start,
            Some(Combo {
                key: Key::Char(' '),
                mods: Mods {
                    ctrl: true,
                    ..Mods::default()
                }
            })
        );
    }

    #[test]
    fn invariant_reserved_defaults_do_not_collide() {
        assert_ne!(
            combo_from_str(DEFAULT_QUIT),
            combo_from_str(DEFAULT_START_MENU)
        );
        for (i, a) in DEFAULTS.iter().enumerate() {
            for b in DEFAULTS.iter().skip(i + 1) {
                let left = combo_from_str(a).expect("default parses");
                let right = combo_from_str(b).expect("default parses");
                assert_ne!(left, right, "reserved defaults {a} and {b} collide");
            }
        }
    }

    #[test]
    fn window_defaults_parse() {
        for spec in [
            DEFAULT_CYCLE_FOCUS,
            DEFAULT_CLOSE_WINDOW,
            DEFAULT_MINIMIZE,
            DEFAULT_MAXIMIZE,
        ] {
            assert!(combo_from_str(spec).is_some(), "{spec} does not parse");
        }
    }

    #[test]
    fn combo_parsing_rejects_unknown_specs() {
        for bad in ["", "hyper-q", "ctrl-", "ab", "ctrl-ab", "q-extra"] {
            assert_eq!(combo_from_str(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn combo_parsing_is_case_insensitive() {
        assert_eq!(combo_from_str("Ctrl-Q"), combo_from_str("ctrl-q"));
        let shift_tab = combo_from_str("Shift-Tab");
        assert_eq!(
            shift_tab,
            Some(Combo {
                key: Key::Tab,
                mods: Mods {
                    shift: true,
                    ..Mods::default()
                }
            })
        );
    }

    #[test]
    fn combo_spelling_round_trips_every_key() {
        for key in ALL_KEYS {
            for spec_mods in [
                Mods::default(),
                Mods {
                    ctrl: true,
                    ..Mods::default()
                },
            ] {
                let combo = Combo {
                    key,
                    mods: spec_mods,
                };
                let text = combo_to_str(combo);
                assert_eq!(combo_from_str(&text), Some(combo), "{text:?} round trip");
            }
        }
        let heavy = combo_to_str(Combo {
            key: Key::Char('q'),
            mods: Mods {
                ctrl: true,
                alt: true,
                shift: true,
            },
        });
        assert_eq!(heavy, "ctrl-alt-shift-q");
    }

    fn plain(key: Key) -> Combo {
        Combo {
            key,
            mods: Mods::default(),
        }
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    /// Every key whose settings spelling round-trips to its canonical
    /// (lowercase-char) form: all named keys, `-` as `minus`, lowercase
    /// and uppercase chars, space, and non-ASCII letters.
    fn spellable_key() -> impl Strategy<Value = Key> {
        prop_oneof![
            8 => "[a-z0-9]".prop_map(|text| Key::Char(text.chars().next().expect("one char"))),
            3 => "[A-Z]".prop_map(|text| Key::Char(text.chars().next().expect("one char"))),
            2 => Just(Key::Char('-')),
            4 => proptest::sample::select(vec![
                Key::Enter,
                Key::Backspace,
                Key::Delete,
                Key::Escape,
                Key::Tab,
                Key::BackTab,
                Key::Left,
                Key::Right,
                Key::Up,
                Key::Down,
                Key::Home,
                Key::End,
                Key::PageUp,
                Key::PageDown,
            ]),
            2 => Just(Key::Char(' ')),
            2 => prop_oneof![
                Just(Key::Char('é')),
                Just(Key::Char('ü')),
                Just(Key::Char('+')),
                Just(Key::Char('.')),
            ],
        ]
    }

    fn any_key() -> impl Strategy<Value = Key> {
        prop_oneof![
            8 => proptest::sample::select(ALL_KEYS.to_vec()),
            1 => Just(Key::Char('-')),
            1 => Just(Key::Char('Q')),
        ]
    }

    fn any_mods() -> impl Strategy<Value = Mods> {
        (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(|(ctrl, alt, shift)| Mods {
            ctrl,
            alt,
            shift,
        })
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// The settings spelling round-trips to the canonical combo
        /// (chars lowercase) for every spellable key and modifier stack.
        #[test]
        fn prop_combo_spelling_round_trips(key in spellable_key(), mods in any_mods()) {
            let combo = Combo { key, mods };
            let text = combo_to_str(combo);
            let mut canonical = combo;
            if let Key::Char(ch) = canonical.key {
                canonical.key = Key::Char(ch.to_ascii_lowercase());
            }
            prop_assert_eq!(
                combo_from_str(&text),
                Some(canonical),
                "{:?} did not round trip", text
            );
        }

        /// The mapping table classifies every (key, mods) pair exactly as
        /// documented: ctrl chars press, alt ignores, plain keys type or
        /// press, everything else ignores.
        #[test]
        fn prop_action_table_is_total_and_classified(key in any_key(), mods in any_mods()) {
            let action = action_for(&Combo { key, mods });
            if mods.ctrl {
                match key {
                    Key::Char(ch) => prop_assert_eq!(
                        action,
                        Action::Press(format!("C-{}", ch.to_ascii_lowercase()))
                    ),
                    _ => prop_assert_eq!(action, Action::Ignore),
                }
                return Ok(());
            }
            if mods.alt {
                prop_assert_eq!(action, Action::Ignore);
                return Ok(());
            }
            match key {
                Key::Char(ch) => prop_assert_eq!(action, Action::Type(ch.to_string())),
                _ => prop_assert!(matches!(action, Action::Press(_)), "{:?} presses nothing", key),
            }
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    /// Audit F1: a bare `-` is the modifier separator, so the char
    /// needs its own key name to be bindable at all.
    #[test]
    fn minus_binds_and_round_trips() {
        let plain = Combo {
            key: Key::Char('-'),
            mods: Mods::default(),
        };
        assert_eq!(combo_to_str(plain), "minus");
        assert_eq!(combo_from_str("minus"), Some(plain));
        let ctrl = Combo {
            key: Key::Char('-'),
            mods: Mods {
                ctrl: true,
                ..Mods::default()
            },
        };
        assert_eq!(combo_from_str("ctrl-minus"), Some(ctrl));
    }

    /// Audit F1: chars spell lowercase, so what the editor persists is
    /// what matching compares (matching normalizes to lowercase).
    #[test]
    fn char_spellings_are_canonical_lowercase() {
        let upper = Combo {
            key: Key::Char('Q'),
            mods: Mods {
                shift: true,
                ..Mods::default()
            },
        };
        assert_eq!(combo_to_str(upper), "shift-q", "chars spell lowercase");
        assert_eq!(
            combo_from_str("shift-q"),
            Some(Combo {
                key: Key::Char('q'),
                mods: upper.mods,
            })
        );
        assert_eq!(
            combo_to_str(Combo {
                key: Key::Char('Q'),
                mods: Mods::default(),
            }),
            "q"
        );
    }

    #[test]
    fn default_quit_combo_matches_the_default_spec() {
        assert_eq!(Some(default_quit_combo()), combo_from_str(DEFAULT_QUIT));
    }
}
