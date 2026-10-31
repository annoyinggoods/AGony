//! Settings file IO and settings-derived decisions. The core owns the
//! model and the TOML format; this module owns the platform paths and the
//! disk.
//!
//! Items: `SettingsIoError` — the IO failure; `Loaded` — settings plus
//! the notes from loading them; `FileStamp` — the (mtime, size) pair the
//! live-reload watcher compares (Part08 8.2.6).
//! Core functions: `config_dir` / `custom_theme_dir` / `desktop_dir` /
//! `bundle_bin_dir` / `home_dir` — the platform paths (shell-resolved,
//! never the core's); `load` — read `<config>/agony/settings.toml`, fall
//! back to defaults with one note per problem, and write the defaults
//! file when none exists yet; `save` — write the whole file on change
//! (temp + rename, so the watcher's parent-dir watch is atomic-rename-
//! safe); `settings_path` / `file_stamp` — the watcher's inputs;
//! `catalog_url` / `catalog_url_in` — the side-effect-free catalog URL
//! read (Part11); `resolve_font_mode` — the runtime auto/nerd/ascii
//! decision; `reserved_combos` — parse the reserved-key specs with
//! collision protection.
//! Depends on: agony_core::config (the model), inputmap (combo parsing).
//! Called by: app (startup, live reload, Apply), Part08's settings UI,
//! thanks (the desktop path), places (the home path).
//! Calls: std::fs under the config dir only.

use std::fs;
use std::path::{Path, PathBuf};

use agony_core::config::{FontMode, Settings};
use agony_core::inputmap::{self, Combo};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SettingsIoError {
    #[error("settings io failed: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Loaded {
    pub settings: Settings,
    pub notes: Vec<String>,
}

/// The platform config directory — the seam's implementation
/// (`platform::config_dir`, Part12 12.2.3).
pub fn config_dir() -> PathBuf {
    crate::platform::config_dir()
}

pub fn custom_theme_dir() -> PathBuf {
    config_dir().join("themes")
}

/// The AGony desktop: `<config>/agony/Desktop` — where THANK-YOU.md lands
/// on first run (Part08 8.4.2) and the icon grid reads (Part09 9.4.1).
pub fn desktop_dir() -> PathBuf {
    config_dir().join("Desktop")
}

/// Where bundle installs land (Part10 §10.3.2): `<config>/agony/bin` —
/// discovery scans it after PATH, so a bundle-installed app turns live.
pub fn bundle_bin_dir() -> PathBuf {
    config_dir().join("bin")
}

/// The settings file's catalog URL (Part11 §11.2.2), read without the
/// side effects of `load` (no defaults written, no notes) — discovery
/// and the CLI verbs read it directly.
pub fn catalog_url() -> String {
    catalog_url_in(&settings_path())
}

/// The same, over an explicit file (the tests' seam).
pub fn catalog_url_in(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    Settings::from_toml(&text)
        .map(|settings| settings.catalog_url)
        .unwrap_or_else(|_| Settings::default().catalog_url)
}

/// The user's home directory — the seam's implementation
/// (`platform::home_dir`); a dot when nothing resolves.
pub fn home_dir() -> PathBuf {
    crate::platform::home_dir()
}

/// The settings file's path (the live-reload watcher watches its parent).
pub fn settings_path() -> PathBuf {
    config_dir().join("settings.toml")
}

/// A file's change stamp: modification time plus length. The watcher
/// compares these to notice external edits — and to ignore its own
/// writes, whose stamp it records right after saving.
pub fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = fs::metadata(path).ok()?;
    Some(FileStamp {
        modified: metadata.modified().ok()?,
        size: metadata.len(),
    })
}

/// The watcher's comparison pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub modified: std::time::SystemTime,
    pub size: u64,
}

/// Read the settings file: a malformed file never prevents startup —
/// defaults load and one note says what was ignored. A missing file loads
/// defaults and writes them out, so the user can discover the format.
pub fn load() -> Loaded {
    let path = config_dir().join("settings.toml");
    let Ok(text) = fs::read_to_string(&path) else {
        // First run: write the defaults out so the format is discoverable;
        // a read-only config dir still boots with defaults.
        let _ = save(&Settings::default());
        return Loaded {
            settings: Settings::default(),
            notes: Vec::new(),
        };
    };
    match Settings::from_toml(&text) {
        Ok(settings) => Loaded {
            settings,
            notes: Vec::new(),
        },
        Err(error) => Loaded {
            settings: Settings::default(),
            notes: vec![format!("settings file ignored ({error}) — defaults loaded")],
        },
    }
}

/// Write the whole file on change. Temp + rename (the atomic-write rule):
/// a watcher watching the parent directory sees one replace event, never
/// a half-written file; a hard kill leaves either the old file or the new.
pub fn save(settings: &Settings) -> Result<(), SettingsIoError> {
    let path = config_dir().join("settings.toml");
    fs::create_dir_all(config_dir())?;
    let temp = config_dir().join("settings.toml.tmp");
    fs::write(&temp, settings.to_toml())?;
    fs::rename(&temp, &path)?;
    Ok(())
}

/// The runtime font decision. `auto` is not decidable headlessly — a
/// terminal does not report its font, and the glyph's cell width is 1
/// either way — so auto resolves to ascii and says so once; the doctor's
/// pattern is how a user decides to pin `nerd`.
pub fn resolve_font_mode(mode: FontMode) -> (FontMode, Option<String>) {
    match mode {
        FontMode::Nerd => (FontMode::Nerd, None),
        FontMode::Ascii => (FontMode::Ascii, None),
        FontMode::Auto => (
            FontMode::Ascii,
            Some(
                "font_mode=auto is not decidable here — using ascii; set font_mode = \"nerd\" \
                 if the doctor's glyph pattern renders for you"
                    .into(),
            ),
        ),
    }
}

/// Every reserved combo the loop matches, in one value (Part04 4.5.1
/// grew the window keys; Part07 added the command bar's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combos {
    pub quit: Combo,
    pub start_menu: Combo,
    pub cycle_focus: Combo,
    pub close_window: Combo,
    pub minimize: Combo,
    pub maximize: Combo,
    pub bar: Combo,
}

impl Combos {
    fn from_defaults() -> Combos {
        let parse = |spec: &str| {
            inputmap::combo_from_str(spec).unwrap_or_else(inputmap::default_quit_combo)
        };
        Combos {
            quit: parse(inputmap::DEFAULT_QUIT),
            start_menu: parse(inputmap::DEFAULT_START_MENU),
            cycle_focus: parse(inputmap::DEFAULT_CYCLE_FOCUS),
            close_window: parse(inputmap::DEFAULT_CLOSE_WINDOW),
            minimize: parse(inputmap::DEFAULT_MINIMIZE),
            maximize: parse(inputmap::DEFAULT_MAXIMIZE),
            bar: parse(inputmap::DEFAULT_BAR),
        }
    }

    fn all(self) -> [Combo; 7] {
        [
            self.quit,
            self.start_menu,
            self.cycle_focus,
            self.close_window,
            self.minimize,
            self.maximize,
            self.bar,
        ]
    }

    fn any_collide(self) -> bool {
        let all = self.all();
        for (i, a) in all.iter().enumerate() {
            if all.iter().skip(i + 1).any(|b| a == b) {
                return true;
            }
        }
        false
    }
}

/// Parse the reserved-key specs; anything unparsable falls back to its
/// default with a note, and any collision falls the whole set back to
/// the defaults with a note (a colliding pair would make one key do two
/// jobs — ambiguous by construction).
pub fn reserved_combos(settings: &Settings) -> (Combos, Vec<String>) {
    let keys = &settings.reserved_keys;
    let mut notes = Vec::new();
    let combos = Combos {
        quit: combo_or_default(&keys.quit, inputmap::DEFAULT_QUIT, &mut notes),
        start_menu: combo_or_default(&keys.start_menu, inputmap::DEFAULT_START_MENU, &mut notes),
        cycle_focus: combo_or_default(&keys.cycle_focus, inputmap::DEFAULT_CYCLE_FOCUS, &mut notes),
        close_window: combo_or_default(
            &keys.close_window,
            inputmap::DEFAULT_CLOSE_WINDOW,
            &mut notes,
        ),
        minimize: combo_or_default(&keys.minimize, inputmap::DEFAULT_MINIMIZE, &mut notes),
        maximize: combo_or_default(&keys.maximize, inputmap::DEFAULT_MAXIMIZE, &mut notes),
        bar: combo_or_default(&keys.bar, inputmap::DEFAULT_BAR, &mut notes),
    };
    if combos.any_collide() {
        return (
            Combos::from_defaults(),
            vec!["reserved keys collide — defaults in use".into()],
        );
    }
    (combos, notes)
}

fn combo_or_default(spec: &str, default: &str, notes: &mut Vec<String>) -> Combo {
    let parsed = inputmap::combo_from_str(spec);
    if let Some(combo) = parsed {
        return combo;
    }
    notes.push(format!(
        "reserved key '{spec}' is not valid — using {default}"
    ));
    inputmap::combo_from_str(default).unwrap_or_else(inputmap::default_quit_combo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agony_core::config::ReservedKeys;

    #[test]
    fn font_auto_resolves_to_ascii_with_one_note() {
        let (mode, note) = resolve_font_mode(FontMode::Auto);
        assert_eq!(mode, FontMode::Ascii);
        assert!(note.is_some());
    }

    #[test]
    fn pinned_font_modes_resolve_to_themselves() {
        assert_eq!(resolve_font_mode(FontMode::Nerd).0, FontMode::Nerd);
        assert_eq!(resolve_font_mode(FontMode::Ascii).0, FontMode::Ascii);
    }

    #[test]
    fn reserved_combos_use_settings_specs() {
        let settings = Settings {
            reserved_keys: ReservedKeys {
                quit: "ctrl-w".into(),
                start_menu: "ctrl-p".into(),
                cycle_focus: "ctrl-tab".into(),
                close_window: "ctrl-x".into(),
                minimize: "ctrl-m".into(),
                maximize: "ctrl-f".into(),
                bar: "ctrl-b".into(),
            },
            ..Settings::default()
        };
        let (combos, notes) = reserved_combos(&settings);
        assert_eq!(
            combos.quit,
            inputmap::combo_from_str("ctrl-w").expect("valid spec")
        );
        assert_eq!(
            combos.start_menu,
            inputmap::combo_from_str("ctrl-p").expect("valid spec")
        );
        assert_eq!(
            combos.bar,
            inputmap::combo_from_str("ctrl-b").expect("valid spec")
        );
        assert!(notes.is_empty());
    }

    #[test]
    fn bad_reserved_keys_fall_back_with_notes() {
        let settings = Settings {
            reserved_keys: ReservedKeys {
                quit: "nope".into(),
                start_menu: "ctrl-space".into(),
                ..ReservedKeys::default()
            },
            ..Settings::default()
        };
        let (combos, notes) = reserved_combos(&settings);
        assert_eq!(
            combos.quit,
            inputmap::combo_from_str(inputmap::DEFAULT_QUIT).expect("default parses")
        );
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn colliding_reserved_keys_fall_back_entirely() {
        let settings = Settings {
            reserved_keys: ReservedKeys {
                quit: "ctrl-w".into(),
                start_menu: "ctrl-space".into(),
                cycle_focus: "ctrl-tab".into(),
                close_window: "ctrl-w".into(),
                minimize: "ctrl-m".into(),
                maximize: "ctrl-f".into(),
                bar: "ctrl-k".into(),
            },
            ..Settings::default()
        };
        let (combos, notes) = reserved_combos(&settings);
        assert!(!combos.any_collide());
        assert!(notes.iter().any(|n| n.contains("collide")), "{notes:?}");
    }

    #[test]
    fn defaults_carry_no_collision() {
        let (combos, notes) = reserved_combos(&Settings::default());
        assert!(!combos.any_collide());
        assert!(notes.is_empty());
    }

    #[test]
    fn catalog_url_reads_the_file_or_falls_back_to_the_default() {
        let dir = std::env::temp_dir().join(format!("agony-settings-url-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let missing = dir.join("none.toml");
        assert_eq!(
            catalog_url_in(&missing),
            Settings::default().catalog_url,
            "a missing file answers the default"
        );
        let path = dir.join("settings.toml");
        std::fs::write(&path, "catalog_url = \"https://example.com/mine.toml\"\n")
            .expect("settings written");
        assert_eq!(catalog_url_in(&path), "https://example.com/mine.toml");
        std::fs::write(&path, "catalog_url = 42\n").expect("junk written");
        assert_eq!(
            catalog_url_in(&path),
            Settings::default().catalog_url,
            "a broken file answers the default, never a failure"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
