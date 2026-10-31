//! The settings model: what persists, its defaults, and TOML
//! (de)serialization.
//!
//! Items: `Settings` — the whole model; `ReservedKeys` — the two combo
//! specs; `TaskbarPosition` / `FontMode` — the two enums settings spell;
//! `ConfigError` — parse/serialize failures.
//! Core functions: `Settings::default` / `from_toml` / `to_toml` — the only
//! settings format handling; unknown fields are ignored, missing fields
//! fall back per-field.
//! Depends on: serde, toml, thiserror.
//! Called by: the shell's `settings` module (it owns the paths and the
//! file IO; the platform is resolved by the shell, never here).
//! Calls: nothing outside this crate.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskbarPosition {
    Bottom,
    Top,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FontMode {
    Nerd,
    Ascii,
    Auto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservedKeys {
    #[serde(default = "default_quit")]
    pub quit: String,
    #[serde(default = "default_start_menu")]
    pub start_menu: String,
    #[serde(default = "default_cycle_focus")]
    pub cycle_focus: String,
    #[serde(default = "default_close_window")]
    pub close_window: String,
    #[serde(default = "default_minimize")]
    pub minimize: String,
    #[serde(default = "default_maximize")]
    pub maximize: String,
    #[serde(default = "default_bar")]
    pub bar: String,
}

/// The file panel's walk knobs (Part05 5.2.3): read once when a refresh
/// starts, never mid-walk.
impl Default for ReservedKeys {
    fn default() -> Self {
        ReservedKeys {
            quit: default_quit(),
            start_menu: default_start_menu(),
            cycle_focus: default_cycle_focus(),
            close_window: default_close_window(),
            minimize: default_minimize(),
            maximize: default_maximize(),
            bar: default_bar(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesSettings {
    pub show_hidden: bool,
    pub respect_ignore: bool,
    pub max_depth: usize,
    pub max_entries: usize,
}

impl Default for FilesSettings {
    fn default() -> Self {
        FilesSettings {
            show_hidden: false,
            respect_ignore: true,
            max_depth: 8,
            max_entries: 2000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(default = "default_theme")]
    pub theme: String,
    pub reserved_keys: ReservedKeys,
    #[serde(default = "default_suggestions")]
    pub suggestions_enabled: bool,
    #[serde(default = "default_taskbar_position")]
    pub taskbar_position: TaskbarPosition,
    #[serde(default = "default_font_mode")]
    pub font_mode: FontMode,
    pub files: FilesSettings,
    /// The app directory's remote catalog URL (Part11 §11.2.2); the
    /// checked-in seed is always merged under it.
    #[serde(default = "default_catalog_url")]
    pub catalog_url: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("settings parse failed: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("settings serialize failed: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: default_theme(),
            reserved_keys: ReservedKeys {
                quit: default_quit(),
                start_menu: default_start_menu(),
                cycle_focus: default_cycle_focus(),
                close_window: default_close_window(),
                minimize: default_minimize(),
                maximize: default_maximize(),
                bar: default_bar(),
            },
            suggestions_enabled: default_suggestions(),
            taskbar_position: default_taskbar_position(),
            font_mode: default_font_mode(),
            files: FilesSettings::default(),
            catalog_url: default_catalog_url(),
        }
    }
}

impl Settings {
    pub fn from_toml(text: &str) -> Result<Settings, ConfigError> {
        Ok(toml::from_str(text)?)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }
}

fn default_theme() -> String {
    "graphite".into()
}

fn default_quit() -> String {
    crate::inputmap::DEFAULT_QUIT.into()
}

fn default_start_menu() -> String {
    crate::inputmap::DEFAULT_START_MENU.into()
}

fn default_cycle_focus() -> String {
    crate::inputmap::DEFAULT_CYCLE_FOCUS.into()
}

fn default_close_window() -> String {
    crate::inputmap::DEFAULT_CLOSE_WINDOW.into()
}

fn default_minimize() -> String {
    crate::inputmap::DEFAULT_MINIMIZE.into()
}

fn default_maximize() -> String {
    crate::inputmap::DEFAULT_MAXIMIZE.into()
}

fn default_bar() -> String {
    crate::inputmap::DEFAULT_BAR.into()
}

fn default_suggestions() -> bool {
    true
}

fn default_taskbar_position() -> TaskbarPosition {
    TaskbarPosition::Bottom
}

fn default_font_mode() -> FontMode {
    FontMode::Auto
}

fn default_catalog_url() -> String {
    "https://raw.githubusercontent.com/mrmflis-hub/AGony/main/catalog/terminal-apps.toml".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        let settings = Settings::default();
        assert_eq!(settings.theme, "graphite");
        assert_eq!(settings.reserved_keys.quit, "ctrl-q");
        assert_eq!(settings.reserved_keys.start_menu, "ctrl-space");
        assert_eq!(settings.reserved_keys.cycle_focus, "ctrl-e");
        assert_eq!(settings.reserved_keys.close_window, "ctrl-w");
        assert_eq!(settings.reserved_keys.minimize, "ctrl-m");
        assert_eq!(settings.reserved_keys.maximize, "ctrl-f");
        assert_eq!(settings.reserved_keys.bar, "ctrl-k");
        assert!(settings.suggestions_enabled);
        assert_eq!(settings.taskbar_position, TaskbarPosition::Bottom);
        assert_eq!(settings.font_mode, FontMode::Auto);
        assert_eq!(settings.files.max_depth, 8);
        assert_eq!(settings.files.max_entries, 2000);
        assert!(settings.files.respect_ignore);
        assert!(!settings.files.show_hidden);
    }

    #[test]
    fn toml_round_trips() {
        let text = Settings::default().to_toml();
        let parsed = Settings::from_toml(&text).expect("own output parses");
        assert_eq!(parsed, Settings::default());
    }

    #[test]
    fn partial_files_keep_per_field_defaults() {
        let parsed = Settings::from_toml("theme = \"paper\"").expect("partial file parses");
        assert_eq!(parsed.theme, "paper");
        assert_eq!(parsed.reserved_keys.quit, "ctrl-q");
        assert_eq!(parsed.font_mode, FontMode::Auto);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let text = "theme = \"paper\"\nfuture_field = 42\n[unknown_section]\nkey = \"v\"\n";
        let parsed = Settings::from_toml(text).expect("unknown fields are not errors");
        assert_eq!(parsed.theme, "paper");
    }

    #[test]
    fn malformed_files_are_errors() {
        assert!(Settings::from_toml("theme = ").is_err());
        assert!(Settings::from_toml("[[[").is_err());
    }

    #[test]
    fn reserved_keys_section_parses() {
        let text = "[reserved_keys]\nquit = \"ctrl-w\"\nstart_menu = \"ctrl-p\"\n";
        let parsed = Settings::from_toml(text).expect("reserved keys parse");
        assert_eq!(parsed.reserved_keys.quit, "ctrl-w");
        assert_eq!(parsed.reserved_keys.start_menu, "ctrl-p");
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// A mutated settings model survives its own TOML round trip.
        #[test]
        fn prop_settings_toml_round_trips(
            theme in "[a-z]{0,10}",
            suggestions in any::<bool>(),
            top in any::<bool>(),
            nerd in 0u8..3,
            files in (any::<bool>(), any::<bool>(), 0..64usize, 0..5000usize),
        ) {
            let settings = Settings {
                theme,
                suggestions_enabled: suggestions,
                taskbar_position: if top {
                    TaskbarPosition::Top
                } else {
                    TaskbarPosition::Bottom
                },
                font_mode: match nerd {
                    0 => FontMode::Nerd,
                    1 => FontMode::Ascii,
                    _ => FontMode::Auto,
                },
                files: FilesSettings {
                    show_hidden: files.0,
                    respect_ignore: files.1,
                    max_depth: files.2,
                    max_entries: files.3,
                },
                ..Settings::default()
            };
            let text = settings.to_toml();
            let parsed = Settings::from_toml(&text).expect("own output parses");
            prop_assert_eq!(parsed, settings);
        }

        /// A partial file with foreign keys parses, fills defaults and
        /// keeps the known field.
        #[test]
        fn prop_partial_toml_keeps_defaults(
            theme in "[a-z]{1,8}",
            foreign in "[a-z_]{1,10}",
            value in 0..1000i64,
        ) {
            let text = format!("theme = \"{theme}\"\nzz_{foreign} = {value}\n");
            let parsed = Settings::from_toml(&text).expect("partial file parses");
            prop_assert_eq!(parsed.theme, theme);
            prop_assert_eq!(parsed.reserved_keys, ReservedKeys::default());
            prop_assert_eq!(parsed.files, FilesSettings::default());
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    #[test]
    fn the_default_catalog_url_is_the_repository_seed() {
        assert_eq!(
            Settings::default().catalog_url,
            "https://raw.githubusercontent.com/mrmflis-hub/AGony/main/catalog/terminal-apps.toml"
        );
    }
}
