//! The theme applier — the only module in the shell that names a color.
//!
//! Every module asks the applier for a role pair; nothing else writes
//! `Color::` (enforced by `tests/source_discipline.rs`).
//! Items: `Applier` — holds the active palette and hands out Styles.
//! Core functions: `Applier::load` — resolve a theme name to a built-in or
//! a validated custom pack, apply the user override pack when it extends
//! that theme (contrast re-checked on the effective palette), falling back
//! to graphite with a note; `Applier::builtin` / `fallback` /
//! `with_parts` — the constructors (the last is the live preview's);
//! `base_theme` — a theme's palette WITHOUT overrides (the picker's
//! effective math starts there); `Applier::name` — the active theme's
//! name; `Applier::style` — one Style per (foreground role, background
//! role) pair.
//! Depends on: agony_core::thememodel (the schema, the validator, the
//! built-ins, the override merge), ratatui Style.
//! Called by: app, desktop, taskbar, startmenu, selftest, settings_ui —
//! every drawn thing.
//! Calls: std::fs only for theme packs and the override pack under the
//! config dir.

use std::fs;
use std::path::Path;

pub use agony_core::thememodel::Role;
use agony_core::thememodel::{self, ColorValue, Palette, Theme};
use ratatui::style::{Color, Style};

/// The user override pack's file name, under the custom theme dir
/// (Part08 8.3.2). Deltas only — never a copy of a whole palette.
pub const OVERRIDE_FILE: &str = "overrides.json";

#[derive(Clone)]
pub struct Applier {
    // Read by `name()`; the settings window tracks names in its draft,
    // so only the tests read it back (the same arrangement as Part07).
    #[allow(dead_code)]
    name: String,
    palette: Palette,
}

impl Applier {
    /// Resolve a theme name: built-in first, then `<custom_dir>/<name>.json`
    /// (parsed and validated by the core — there is no other way to read a
    /// theme file). `<custom_dir>/overrides.json` then applies its deltas
    /// when it extends this theme; a malformed or clashing override keeps
    /// the base palette and says why. Any failure falls back to graphite
    /// and says why.
    pub fn load(name: &str, custom_dir: Option<&Path>) -> (Applier, Vec<String>) {
        let Some(base) = base_theme(name, custom_dir) else {
            return not_found(name);
        };
        let (palette, notes) = apply_overrides(base.palette, name, custom_dir);
        (
            Applier {
                name: base.name,
                palette,
            },
            notes,
        )
    }

    /// An applier over an explicit palette — the live preview's
    /// constructor: no files are read, nothing persists (Part08's
    /// preview-until-commit rule).
    pub fn with_parts(name: &str, palette: Palette) -> Applier {
        Applier {
            name: name.to_string(),
            palette,
        }
    }

    pub fn builtin(name: &str) -> Option<Applier> {
        let theme = thememodel::builtin(name)?;
        Some(Applier {
            name: theme.name,
            palette: theme.palette,
        })
    }

    /// Graphite — what every fallback lands on.
    pub fn fallback() -> Applier {
        Applier::builtin("graphite").unwrap_or_else(fallback_constant)
    }

    /// The active theme's name — tests pin it.
    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// One Style per (fg role, bg role) pair — the only color decision
    /// point in the shell.
    pub fn style(&self, fg: Role, bg: Role) -> Style {
        Style::new().fg(self.color(fg)).bg(self.color(bg))
    }

    fn color(&self, role: Role) -> Color {
        color_of(self.palette.get(role))
    }
}

/// The one ColorValue → ratatui Color mapping — the applier's own, and
/// the picker's preview strips reuse it (no second Color:: site).
pub fn color_of(value: ColorValue) -> Color {
    match value {
        ColorValue::Indexed(index) => Color::Indexed(index),
        ColorValue::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn not_found(name: &str) -> (Applier, Vec<String>) {
    (
        Applier::fallback(),
        vec![format!("theme '{name}' not found — using graphite")],
    )
}

/// A theme's base palette by name, WITHOUT the override pack: built-in
/// first, then `<custom_dir>/<name>.json` (validated by the core). The
/// settings window's effective math starts here.
pub fn base_theme(name: &str, custom_dir: Option<&Path>) -> Option<Theme> {
    if let Some(theme) = thememodel::builtin(name) {
        return Some(theme);
    }
    let dir = custom_dir?;
    let text = fs::read_to_string(dir.join(format!("{name}.json"))).ok()?;
    thememodel::parse_theme(&text).ok()
}

/// Base + the override pack's deltas (when the pack extends `name`),
/// contrast-checked on the effective palette. A malformed or refused
/// override keeps the base palette and says so — the malformed-settings
/// rule; a pack extending another theme is simply inert here.
fn apply_overrides(base: Palette, name: &str, custom_dir: Option<&Path>) -> (Palette, Vec<String>) {
    let Some(dir) = custom_dir else {
        return (base, Vec::new());
    };
    let Ok(text) = fs::read_to_string(dir.join(OVERRIDE_FILE)) else {
        return (base, Vec::new());
    };
    let overrides = match thememodel::parse_override(&text) {
        Ok(overrides) => overrides,
        Err(error) => return (base, vec![format!("theme overrides ignored ({error})")]),
    };
    if overrides.extends != name {
        return (base, Vec::new());
    }
    let effective = thememodel::effective_palette(&base, &overrides.roles);
    if let Err(error) = thememodel::validate(&effective) {
        return (base, vec![format!("theme overrides rejected ({error})")]);
    }
    (effective, Vec::new())
}

/// Unreachable: graphite is a compile-time constant. Keeps `fallback()`
/// total without `expect`.
fn fallback_constant() -> Applier {
    let palette = thememodel::builtin("graphite")
        .map(|theme| theme.palette)
        .unwrap_or(Palette {
            background: ColorValue::Indexed(16),
            surface: ColorValue::Indexed(17),
            text: ColorValue::Indexed(231),
            text_dim: ColorValue::Indexed(145),
            border: ColorValue::Indexed(59),
            accent: ColorValue::Indexed(156),
            danger: ColorValue::Indexed(210),
            focus: ColorValue::Indexed(23),
            selection: ColorValue::Indexed(11),
            titlebar_active: ColorValue::Indexed(60),
            titlebar_idle: ColorValue::Indexed(59),
        });
    Applier {
        name: "graphite".into(),
        palette,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_load_without_notes() {
        for name in thememodel::BUILTIN_NAMES {
            let (applier, notes) = Applier::load(name, None);
            assert_eq!(applier.name(), name);
            assert!(notes.is_empty(), "{name} produced notes: {notes:?}");
        }
    }

    #[test]
    fn unknown_names_fall_back_to_graphite_with_one_note() {
        let (applier, notes) = Applier::load("nope", None);
        assert_eq!(applier.name(), "graphite");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("nope"), "note names the theme: {notes:?}");
    }

    #[test]
    fn broken_theme_files_fall_back_with_one_note() {
        let dir = std::env::temp_dir().join("agony-theme-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("broken.json"), "{ not json").expect("temp file");
        let (applier, notes) = Applier::load("broken", Some(&dir));
        assert_eq!(applier.name(), "graphite");
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].contains("broken"),
            "note names the theme: {notes:?}"
        );
    }

    #[test]
    fn styles_carry_role_colors() {
        let (applier, _) = Applier::load("contrast", None);
        let style = applier.style(Role::Text, Role::Background);
        assert_eq!(style.fg, Some(Color::Rgb(255, 255, 255)));
        assert_eq!(style.bg, Some(Color::Rgb(0, 0, 0)));
    }

    // ------------------------------------------------- overrides (Part08)

    fn override_dir(tag: &str, extends: &str, roles: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("agony-ovr-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let json =
            format!(r##"{{"name":"user overrides","extends":"{extends}","roles":{roles}}}"##);
        std::fs::write(dir.join(OVERRIDE_FILE), &json).expect("temp file");
        dir
    }

    #[test]
    fn extending_overrides_recolor_the_load() {
        let dir = override_dir("recolor", "graphite", r##"{"accent":"#66d9ff"}"##);
        let (applier, notes) = Applier::load("graphite", Some(&dir));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            applier.style(Role::Accent, Role::Background).fg,
            Some(Color::Rgb(0x66, 0xd9, 0xff)),
            "the delta reached the applier"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overrides_for_another_theme_are_inert() {
        let dir = override_dir("inert", "paper", r##"{"accent":"#66d9ff"}"##);
        let (applier, notes) = Applier::load("graphite", Some(&dir));
        assert!(notes.is_empty());
        let (plain, _) = Applier::load("graphite", None);
        assert_eq!(
            applier.style(Role::Accent, Role::Background).fg,
            plain.style(Role::Accent, Role::Background).fg,
            "another theme's deltas never leak"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contrast_failing_overrides_keep_the_base_and_say_so() {
        let dir = override_dir("mud", "graphite", r##"{"text":"#1b1e24"}"##);
        let (applier, notes) = Applier::load("graphite", Some(&dir));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("rejected"), "{notes:?}");
        let (base, _) = Applier::load("graphite", None);
        assert_eq!(
            applier.style(Role::Text, Role::Background).fg,
            base.style(Role::Text, Role::Background).fg,
            "the base palette survived"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_overrides_keep_the_base_and_say_so() {
        let dir = std::env::temp_dir().join(format!("agony-ovr-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join(OVERRIDE_FILE), "{ not json").expect("temp file");
        let (applier, notes) = Applier::load("graphite", Some(&dir));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("ignored"), "{notes:?}");
        let (base, _) = Applier::load("graphite", None);
        assert_eq!(
            applier.style(Role::Background, Role::Background).bg,
            base.style(Role::Background, Role::Background).bg
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base_theme_reads_builtins_and_custom_files_without_overrides() {
        assert!(base_theme("graphite", None).is_some());
        assert!(base_theme("nope", None).is_none());
        let dir = std::env::temp_dir().join(format!("agony-theme-base-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("customx.json"), "{ not json").expect("temp file");
        assert!(
            base_theme("customx", Some(&dir)).is_none(),
            "broken file is none"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_parts_builds_a_preview_without_files() {
        let palette = thememodel::builtin("paper").expect("paper").palette;
        let applier = Applier::with_parts("paper", palette);
        assert_eq!(applier.name(), "paper");
        assert_eq!(
            applier.style(Role::Text, Role::Background).fg,
            Some(Color::Rgb(0x23, 0x27, 0x2e))
        );
    }
}
