//! The palette schema, the built-in themes, the contrast arithmetic and the
//! JSON theme-pack loader/validator.
//!
//! Items: `ColorValue` — a 256-color index or an RGB triple (serializes as
//! an index number or a `"#rrggbb"` string); `Role` — a named palette slot
//! (`ALL_ROLES` — every slot, one place); `Palette` — all roles filled;
//! `Theme` — a named palette; `OverridePack` / `Override` — the user
//! override pack (`extends` names the base theme; the roles are deltas);
//! `ThemeError` — every way loading can fail.
//! Core functions: `contrast_ratio` — the one WCAG ratio function;
//! `validate` — refuse palettes whose text-on-surface pairs fail 4.5:1;
//! `parse_theme` — the only way theme JSON becomes a Theme; `parse_override`
//! / `override_to_json` — the override pack's two directions;
//! `effective_palette` — base + override deltas merged (contrast runs on
//! the result, never on the deltas alone); `builtin` / `BUILTIN_NAMES` —
//! the three built-ins (graphite, paper, contrast); `role_name` /
//! `role_from_name` — the role spellings; `Palette::get` / `Palette::set` /
//! `Palette::from_pairs` — slot access and construction.
//! Depends on: serde/serde_json (the pack format), thiserror.
//! Called by: the shell's `theme` applier; Part08's settings window and
//! color picker.
//! Calls: nothing outside this crate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorValue {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// JSON shape: `16` (a 256-color index) or `"#1b1e24"` (a hex triple) —
/// the same two spellings `parse_theme` has always read, now in one impl.
impl Serialize for ColorValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match *self {
            ColorValue::Indexed(index) => serializer.serialize_u8(index),
            ColorValue::Rgb(r, g, b) => {
                serializer.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}"))
            }
        }
    }
}

impl<'de> Deserialize<'de> for ColorValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<ColorValue, D::Error> {
        struct ColorVisitor;
        impl serde::de::Visitor<'_> for ColorVisitor {
            type Value = ColorValue;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a 0-255 color index or a \"#rrggbb\" string")
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<ColorValue, E> {
                u8::try_from(v)
                    .map(ColorValue::Indexed)
                    .map_err(|_| E::custom(format!("color index {v} exceeds 255")))
            }
            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<ColorValue, E> {
                parse_hex(text).map_err(|error| E::custom(error.to_string()))
            }
        }
        deserializer.deserialize_any(ColorVisitor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Background,
    Surface,
    Text,
    TextDim,
    Border,
    Accent,
    Danger,
    Focus,
    Selection,
    TitlebarActive,
    TitlebarIdle,
}

pub const ALL_ROLES: [Role; 11] = [
    Role::Background,
    Role::Surface,
    Role::Text,
    Role::TextDim,
    Role::Border,
    Role::Accent,
    Role::Danger,
    Role::Focus,
    Role::Selection,
    Role::TitlebarActive,
    Role::TitlebarIdle,
];

pub const BUILTIN_NAMES: [&str; 3] = ["graphite", "paper", "contrast"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: ColorValue,
    pub surface: ColorValue,
    pub text: ColorValue,
    pub text_dim: ColorValue,
    pub border: ColorValue,
    pub accent: ColorValue,
    pub danger: ColorValue,
    pub focus: ColorValue,
    pub selection: ColorValue,
    pub titlebar_active: ColorValue,
    pub titlebar_idle: ColorValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub palette: Palette,
}

#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("theme json is malformed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown role '{0}'")]
    UnknownRole(String),
    #[error("missing roles: {0}")]
    MissingRoles(String),
    #[error("bad color '{0}' (want 0-255 or \"#rrggbb\")")]
    BadColor(String),
    #[error("contrast {ratio:.2}:1 is below 4.5:1 for {pair}")]
    LowContrast { pair: String, ratio: f64 },
}

impl Palette {
    pub fn get(&self, role: Role) -> ColorValue {
        match role {
            Role::Background => self.background,
            Role::Surface => self.surface,
            Role::Text => self.text,
            Role::TextDim => self.text_dim,
            Role::Border => self.border,
            Role::Accent => self.accent,
            Role::Danger => self.danger,
            Role::Focus => self.focus,
            Role::Selection => self.selection,
            Role::TitlebarActive => self.titlebar_active,
            Role::TitlebarIdle => self.titlebar_idle,
        }
    }

    /// Overwrite one slot (the effective-palette merge's building block).
    pub fn set(&mut self, role: Role, value: ColorValue) {
        match role {
            Role::Background => self.background = value,
            Role::Surface => self.surface = value,
            Role::Text => self.text = value,
            Role::TextDim => self.text_dim = value,
            Role::Border => self.border = value,
            Role::Accent => self.accent = value,
            Role::Danger => self.danger = value,
            Role::Focus => self.focus = value,
            Role::Selection => self.selection = value,
            Role::TitlebarActive => self.titlebar_active = value,
            Role::TitlebarIdle => self.titlebar_idle = value,
        }
    }

    /// Build a palette from role/value pairs; every role must be present.
    pub fn from_pairs(pairs: &[(Role, ColorValue)]) -> Result<Palette, ThemeError> {
        let map: BTreeMap<Role, ColorValue> = pairs.iter().copied().collect();
        let missing: Vec<&str> = ALL_ROLES
            .iter()
            .filter(|role| !map.contains_key(role))
            .map(|role| role_name(*role))
            .collect();
        if !missing.is_empty() {
            return Err(ThemeError::MissingRoles(missing.join(", ")));
        }
        // The missing-roles check above guarantees every lookup hits; `.get`
        // with a black-indexed fallback keeps the constructor panic-free by
        // construction regardless (audit F5).
        let at = |role: Role| map.get(&role).copied().unwrap_or(ColorValue::Indexed(0));
        Ok(Palette {
            background: at(Role::Background),
            surface: at(Role::Surface),
            text: at(Role::Text),
            text_dim: at(Role::TextDim),
            border: at(Role::Border),
            accent: at(Role::Accent),
            danger: at(Role::Danger),
            focus: at(Role::Focus),
            selection: at(Role::Selection),
            titlebar_active: at(Role::TitlebarActive),
            titlebar_idle: at(Role::TitlebarIdle),
        })
    }
}

pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Background => "background",
        Role::Surface => "surface",
        Role::Text => "text",
        Role::TextDim => "text-dim",
        Role::Border => "border",
        Role::Accent => "accent",
        Role::Danger => "danger",
        Role::Focus => "focus",
        Role::Selection => "selection",
        Role::TitlebarActive => "titlebar-active",
        Role::TitlebarIdle => "titlebar-idle",
    }
}

pub fn role_from_name(name: &str) -> Option<Role> {
    ALL_ROLES.iter().find(|r| role_name(**r) == name).copied()
}

/// The one contrast function: WCAG relative luminance ratio, higher is
/// better, same color = 1.0, black/white = 21.0.
pub fn contrast_ratio(a: ColorValue, b: ColorValue) -> f64 {
    let (la, lb) = (luminance(color_to_rgb(a)), luminance(color_to_rgb(b)));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Refuse a palette whose declared text-on-surface pairs fail 4.5:1.
pub fn validate(palette: &Palette) -> Result<(), ThemeError> {
    const TEXT_PAIRS: [(Role, Role); 10] = [
        (Role::Text, Role::Background),
        (Role::Text, Role::Surface),
        (Role::TextDim, Role::Background),
        (Role::TextDim, Role::Surface),
        (Role::Accent, Role::Background),
        (Role::Danger, Role::Background),
        (Role::Background, Role::Accent),
        (Role::Background, Role::Selection),
        (Role::Text, Role::TitlebarActive),
        (Role::Text, Role::TitlebarIdle),
    ];
    for (fg, bg) in TEXT_PAIRS {
        let ratio = contrast_ratio(palette.get(fg), palette.get(bg));
        let readable = ratio >= 4.5;
        if !readable {
            return Err(ThemeError::LowContrast {
                pair: format!("{} on {}", role_name(fg), role_name(bg)),
                ratio,
            });
        }
    }
    Ok(())
}

/// The only way theme-pack JSON becomes a Theme: parse, fill every role,
/// validate contrast. A theme that cannot be checked is not loaded.
/// Packs written before the `selection` role keep loading: a missing
/// selection slot defaults to the accent it used to share its job with.
pub fn parse_theme(json: &str) -> Result<Theme, ThemeError> {
    let pack: ThemePack = serde_json::from_str(json)?;
    let mut pairs = Vec::new();
    for (name, color) in &pack.roles {
        let role = role_from_name(name).ok_or_else(|| ThemeError::UnknownRole(name.clone()))?;
        pairs.push((role, *color));
    }
    let has_selection = pairs.iter().any(|(role, _)| *role == Role::Selection);
    if !has_selection {
        let accent = pairs
            .iter()
            .find(|(role, _)| *role == Role::Accent)
            .map(|(_, color)| *color)
            .unwrap_or(ColorValue::Indexed(0));
        pairs.push((Role::Selection, accent));
    }
    let palette = Palette::from_pairs(&pairs)?;
    validate(&palette)?;
    Ok(Theme {
        name: pack.name,
        palette,
    })
}

/// The override pack's file schema: the same roles map as a theme pack,
/// plus `extends` naming the base theme the deltas apply over. Only the
/// changed roles appear — a theme-pack update still flows through.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OverridePack {
    pub name: String,
    pub extends: String,
    #[serde(default)]
    pub roles: BTreeMap<String, ColorValue>,
}

/// A parsed override: the base theme's name plus role deltas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    pub extends: String,
    pub roles: Vec<(Role, ColorValue)>,
}

/// The only way override-pack JSON becomes an Override: parse, resolve
/// every role name. Contrast is not checked here — it runs on the
/// *effective* palette (see [`effective_palette`]), which needs the base.
pub fn parse_override(json: &str) -> Result<Override, ThemeError> {
    let pack: OverridePack = serde_json::from_str(json)?;
    let mut roles = Vec::new();
    for (name, color) in &pack.roles {
        let role = role_from_name(name).ok_or_else(|| ThemeError::UnknownRole(name.clone()))?;
        roles.push((role, *color));
    }
    Ok(Override {
        extends: pack.extends,
        roles,
    })
}

/// Serialize an override pack (the color picker's commit writes it).
pub fn override_to_json(name: &str, extends: &str, pairs: &[(Role, ColorValue)]) -> String {
    let pack = OverridePack {
        name: name.to_string(),
        extends: extends.to_string(),
        roles: pairs
            .iter()
            .map(|(role, color)| (role_name(*role).to_string(), *color))
            .collect(),
    };
    serde_json::to_string_pretty(&pack).unwrap_or_default()
}

/// Base + override deltas. The input pairs must already carry valid roles
/// (`parse_override`'s output); contrast runs on the result, never here.
pub fn effective_palette(base: &Palette, overrides: &[(Role, ColorValue)]) -> Palette {
    let mut palette = base.clone();
    for (role, value) in overrides {
        palette.set(*role, *value);
    }
    palette
}

pub fn builtin(name: &str) -> Option<Theme> {
    let theme = match name {
        "graphite" => graphite(),
        "paper" => paper(),
        "contrast" => contrast(),
        _ => return None,
    };
    Some(theme)
}

#[derive(Deserialize)]
struct ThemePack {
    name: String,
    roles: BTreeMap<String, ColorValue>,
}

/// The "#rrggbb" spelling of a color (the hex input line's display form;
/// the Serialize impl shares it). Indexed colors do not spell as hex —
/// they are their own notation.
pub fn hex_of(value: ColorValue) -> String {
    match value {
        ColorValue::Indexed(index) => format!("{index}"),
        ColorValue::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

/// Parse a "#rrggbb" (or bare-6-digit) hex string. Public for the
/// settings window's hex input line — the one parser, core-side.
pub fn parse_hex(text: &str) -> Result<ColorValue, ThemeError> {
    let digits = text.strip_prefix('#').unwrap_or(text);
    let invalid = || ThemeError::BadColor(text.to_string());
    let (Ok(r), Ok(g), Ok(b), true) = (
        u8::from_str_radix(digits.get(0..2).ok_or_else(invalid)?, 16),
        u8::from_str_radix(digits.get(2..4).ok_or_else(invalid)?, 16),
        u8::from_str_radix(digits.get(4..6).ok_or_else(invalid)?, 16),
        digits.len() == 6,
    ) else {
        return Err(invalid());
    };
    Ok(ColorValue::Rgb(r, g, b))
}

fn color_to_rgb(color: ColorValue) -> (u8, u8, u8) {
    match color {
        ColorValue::Indexed(index) => indexed_to_rgb(index),
        ColorValue::Rgb(r, g, b) => (r, g, b),
    }
}

/// The xterm 256-color palette, so indexed colors get real contrast math.
fn indexed_to_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => BASE16[index as usize],
        16..=231 => cube_to_rgb(index - 16),
        _ => gray_to_rgb(index - 232),
    }
}

const BASE16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0x80, 0x00, 0x00),
    (0x00, 0x80, 0x00),
    (0x80, 0x80, 0x00),
    (0x00, 0x00, 0x80),
    (0x80, 0x00, 0x80),
    (0x00, 0x80, 0x80),
    (0xc0, 0xc0, 0xc0),
    (0x80, 0x80, 0x80),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x00, 0x00, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];

fn cube_to_rgb(i: u8) -> (u8, u8, u8) {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    (
        STEPS[(i / 36) as usize],
        STEPS[((i / 6) % 6) as usize],
        STEPS[(i % 6) as usize],
    )
}

fn gray_to_rgb(i: u8) -> (u8, u8, u8) {
    let v = (u16::from(i) * 10 + 8).min(255) as u8;
    (v, v, v)
}

fn channel_to_linear(c: u8) -> f64 {
    let v = f64::from(c) / 255.0;
    if v <= 0.039_28 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(rgb: (u8, u8, u8)) -> f64 {
    0.2126 * channel_to_linear(rgb.0)
        + 0.7152 * channel_to_linear(rgb.1)
        + 0.0722 * channel_to_linear(rgb.2)
}

/// Build a palette value from hex literals without runtime parsing — the
/// built-in themes are constants, not files.
const fn rgb(r: u8, g: u8, b: u8) -> ColorValue {
    ColorValue::Rgb(r, g, b)
}

/// Graphite — the default dark theme. The user-specified secondary pair:
/// selection `#ffff00` (the loud highlight), focus `#c87137` (the window
/// border) — distinct from the green accent, per the UI review's
/// one-color-three-meanings finding.
fn graphite() -> Theme {
    Theme {
        name: "graphite".into(),
        palette: Palette {
            background: rgb(0x1b, 0x1e, 0x24),
            surface: rgb(0x24, 0x28, 0x30),
            text: rgb(0xdf, 0xe3, 0xea),
            text_dim: rgb(0x98, 0xa0, 0xad),
            border: rgb(0x3a, 0x41, 0x50),
            accent: rgb(0x7e, 0xe0, 0xa3),
            danger: rgb(0xff, 0x80, 0x80),
            focus: rgb(0xc8, 0x71, 0x37),
            selection: rgb(0xff, 0xff, 0x00),
            titlebar_active: rgb(0x34, 0x3b, 0x49),
            titlebar_idle: rgb(0x25, 0x2a, 0x34),
        },
    }
}

/// Paper — the light theme. The user-specified secondary pair, assigned
/// by legibility: `#c87137` fails the 4.5:1 text gate as a selection
/// background here (4.38:1), so it takes the focus border and `#216778`
/// (5.6:1 with the paper background as text) takes selection.
fn paper() -> Theme {
    Theme {
        name: "paper".into(),
        palette: Palette {
            background: rgb(0xf2, 0xf0, 0xeb),
            surface: rgb(0xe6, 0xe2, 0xda),
            text: rgb(0x23, 0x27, 0x2e),
            text_dim: rgb(0x56, 0x5e, 0x6b),
            border: rgb(0xb9, 0xb2, 0xa4),
            accent: rgb(0x10, 0x6b, 0x4a),
            danger: rgb(0xb0, 0x30, 0x30),
            focus: rgb(0xc8, 0x71, 0x37),
            selection: rgb(0x21, 0x67, 0x78),
            titlebar_active: rgb(0xd9, 0xd4, 0xc8),
            titlebar_idle: rgb(0xe4, 0xdf, 0xd4),
        },
    }
}

/// Contrast — the high-contrast dark theme.
fn contrast() -> Theme {
    Theme {
        name: "contrast".into(),
        palette: Palette {
            background: rgb(0x00, 0x00, 0x00),
            surface: rgb(0x10, 0x10, 0x10),
            text: rgb(0xff, 0xff, 0xff),
            text_dim: rgb(0xc8, 0xc8, 0xc8),
            border: rgb(0x80, 0x80, 0x80),
            accent: rgb(0x00, 0xff, 0x66),
            danger: rgb(0xff, 0x50, 0x50),
            focus: rgb(0x00, 0x45, 0x26),
            selection: rgb(0xff, 0xff, 0x00),
            titlebar_active: rgb(0x33, 0x33, 0x33),
            titlebar_idle: rgb(0x1f, 0x1f, 0x1f),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_ratio_black_on_white_is_21() {
        let white = ColorValue::Rgb(255, 255, 255);
        let black = ColorValue::Rgb(0, 0, 0);
        let ratio = contrast_ratio(white, black);
        assert!((ratio - 21.0).abs() < 0.01, "ratio was {ratio}");
    }

    #[test]
    fn contrast_ratio_of_a_color_with_itself_is_one() {
        let color = ColorValue::Rgb(120, 40, 200);
        assert!((contrast_ratio(color, color) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn contrast_ratio_understands_indexed_colors() {
        let white = ColorValue::Indexed(15);
        let black = ColorValue::Indexed(0);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
    }

    #[test]
    fn invariant_every_builtin_validates() {
        for name in BUILTIN_NAMES {
            let theme = builtin(name).expect("builtin exists");
            assert!(validate(&theme.palette).is_ok(), "{name} failed validation");
        }
    }

    #[test]
    fn unknown_builtin_name_is_none() {
        assert!(builtin("nope").is_none());
    }

    #[test]
    fn parse_theme_accepts_a_valid_pack_with_indexed_colors() {
        let json = r##"{
            "name": "custom-dark",
            "roles": {
                "background": 16, "surface": 17, "text": 231, "text-dim": 145,
                "border": 59, "accent": 156, "danger": 210, "focus": 23,
                "titlebar-active": 60, "titlebar-idle": 59
            }
        }"##;
        let theme = parse_theme(json).expect("valid pack parses");
        assert_eq!(theme.name, "custom-dark");
        assert_eq!(theme.palette.get(Role::Background), ColorValue::Indexed(16));
    }

    #[test]
    fn parse_theme_refuses_a_low_contrast_palette() {
        let json = r##"{
            "name": "mud",
            "roles": {
                "background": "#777777", "surface": "#707070", "text": "#808080",
                "text-dim": "#7a7a7a", "border": "#909090", "accent": "#ffffff",
                "danger": "#ffffff", "focus": "#505050",
                "titlebar-active": "#606060", "titlebar-idle": "#656565"
            }
        }"##;
        let error = parse_theme(json).expect_err("low contrast must be refused");
        assert!(matches!(error, ThemeError::LowContrast { .. }), "{error}");
    }

    #[test]
    fn parse_theme_refuses_missing_roles() {
        let json = r##"{"name": "holes", "roles": {"background": "#000000"}}"##;
        let error = parse_theme(json).expect_err("missing roles must be refused");
        assert!(matches!(error, ThemeError::MissingRoles(_)), "{error}");
    }

    #[test]
    fn parse_theme_refuses_unknown_roles() {
        let json = r##"{"name": "odd", "roles": {"sparkle": "#ffffff"}}"##;
        let error = parse_theme(json).expect_err("unknown role must be refused");
        assert!(matches!(error, ThemeError::UnknownRole(_)), "{error}");
    }

    #[test]
    fn parse_theme_refuses_bad_colors() {
        let json = r##"{"name": "bad", "roles": {"background": "#zzzzzz"}}"##;
        let error = parse_theme(json).expect_err("bad hex must be refused");
        // The color visitor surfaces parse_hex's refusal through serde's
        // error wrapper, naming the color either way.
        assert!(error.to_string().contains("bad color '#zzzzzz'"), "{error}");
    }

    #[test]
    fn parse_theme_refuses_malformed_json() {
        let error = parse_theme("{ not json").expect_err("malformed json must be refused");
        assert!(matches!(error, ThemeError::Json(_)), "{error}");
    }

    #[test]
    fn role_names_round_trip() {
        for role in ALL_ROLES {
            assert_eq!(role_from_name(role_name(role)), Some(role));
        }
    }

    // ------------------------------------------------------- overrides

    #[test]
    fn effective_palette_changes_only_the_named_roles() {
        let base = builtin("graphite").expect("graphite").palette;
        let overrides = [(Role::Accent, ColorValue::Rgb(255, 0, 0))];
        let effective = effective_palette(&base, &overrides);
        assert_eq!(effective.get(Role::Accent), ColorValue::Rgb(255, 0, 0));
        for role in ALL_ROLES {
            if role != Role::Accent {
                assert_eq!(
                    effective.get(role),
                    base.get(role),
                    "{role:?} must flow through"
                );
            }
        }
    }

    #[test]
    fn contrast_runs_on_the_effective_palette_and_names_the_pair() {
        let base = builtin("graphite").expect("graphite").palette;
        // Accent forced onto the background's own color: a guaranteed fail.
        let overrides = [(Role::Accent, base.get(Role::Background))];
        let effective = effective_palette(&base, &overrides);
        let error = validate(&effective).expect_err("a mud-on-mud accent is refused");
        let ThemeError::LowContrast { pair, .. } = error else {
            panic!("expected LowContrast");
        };
        assert_eq!(pair, "accent on background");
    }

    #[test]
    fn a_good_override_leaves_the_effective_palette_valid() {
        let base = builtin("graphite").expect("graphite").palette;
        let overrides = [(Role::Accent, ColorValue::Rgb(120, 200, 255))];
        let effective = effective_palette(&base, &overrides);
        validate(&effective).expect("a readable accent validates");
    }

    #[test]
    fn override_pack_round_trips_through_json() {
        let pairs = [
            (Role::Accent, ColorValue::Rgb(255, 0, 0)),
            (Role::Border, ColorValue::Indexed(59)),
        ];
        let json = override_to_json("user overrides", "graphite", &pairs);
        let parsed = parse_override(&json).expect("own output parses");
        assert_eq!(parsed.extends, "graphite");
        assert_eq!(parsed.roles, pairs.to_vec());
    }

    #[test]
    fn override_refuses_unknown_roles_and_bad_json() {
        let bad_role = r##"{"name":"o","extends":"graphite","roles":{"sparkle":"#ffffff"}}"##;
        assert!(matches!(
            parse_override(bad_role),
            Err(ThemeError::UnknownRole(_))
        ));
        assert!(matches!(
            parse_override("{ not json"),
            Err(ThemeError::Json(_))
        ));
    }

    #[test]
    fn override_requires_the_extends_field() {
        let orphan = r##"{"name":"o","roles":{"accent":"#ffffff"}}"##;
        let error = parse_override(orphan).expect_err("extends is required");
        assert!(matches!(error, ThemeError::Json(_)), "{error}");
    }

    #[test]
    fn override_accepts_hex_strings_and_indexes() {
        let json = r##"{"name":"o","extends":"paper","roles":{"accent":"#106b4a","border":59}}"##;
        let parsed = parse_override(json).expect("both spellings parse");
        assert_eq!(
            parsed.roles,
            vec![
                (Role::Accent, ColorValue::Rgb(0x10, 0x6b, 0x4a)),
                (Role::Border, ColorValue::Indexed(59)),
            ]
        );
    }

    #[test]
    fn color_value_serializes_compactly() {
        let json = serde_json::to_string(&ColorValue::Indexed(59)).expect("index");
        assert_eq!(json, "59");
        let json = serde_json::to_string(&ColorValue::Rgb(0x10, 0x6b, 0x4a)).expect("hex");
        assert_eq!(json, "\"#106b4a\"");
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    fn color_strategy() -> impl Strategy<Value = ColorValue> {
        prop_oneof![
            any::<u8>().prop_map(ColorValue::Indexed),
            (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b)| ColorValue::Rgb(r, g, b)),
        ]
    }

    fn palette_of(colors: [ColorValue; 11]) -> Palette {
        let pairs: Vec<(Role, ColorValue)> = ALL_ROLES.into_iter().zip(colors).collect();
        Palette::from_pairs(&pairs).expect("all roles present")
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// The WCAG ratio is symmetric, at least 1, at most 21, and 1
        /// against itself.
        #[test]
        fn prop_contrast_is_symmetric_bounded_and_reflexive(
            a in color_strategy(),
            b in color_strategy(),
        ) {
            let ab = contrast_ratio(a, b);
            let ba = contrast_ratio(b, a);
            prop_assert!((ab - ba).abs() < 1e-9, "not symmetric: {ab} vs {ba}");
            prop_assert!((1.0..=21.0 + 1e-6).contains(&ab), "ratio {ab} out of range");
            prop_assert!((contrast_ratio(a, a) - 1.0).abs() < 1e-9);
        }

        /// Hex spelling round-trips, with or without the hash; hex
        /// strings of any other length are refused.
        #[test]
        fn prop_parse_hex_round_trips_and_refuses_lengths(
            digits in "[0-9a-fA-F]{0,12}",
            hashed in any::<bool>(),
        ) {
            let text = if hashed {
                format!("#{digits}")
            } else {
                digits.clone()
            };
            let parsed = parse_hex(&text);
            prop_assert_eq!(parsed.is_ok(), digits.len() == 6, "hex {:?}", text);
            let Ok(value) = parsed else {
                return Ok(());
            };
            let ColorValue::Rgb(r, g, b) = value else {
                panic!("hex parses to rgb");
            };
            prop_assert_eq!(hex_of(ColorValue::Rgb(r, g, b)), format!("#{r:02x}{g:02x}{b:02x}"));
            let again = parse_hex(&hex_of(ColorValue::Rgb(r, g, b)));
            prop_assert_eq!(again.expect("round trip"), ColorValue::Rgb(r, g, b));
        }

        /// effective_palette applies overrides in order, last write wins.
        #[test]
        fn prop_effective_palette_last_write_wins(
            colors in proptest::collection::vec(color_strategy(), ALL_ROLES.len()),
            overrides in proptest::collection::vec(
                (proptest::sample::select(ALL_ROLES.to_vec()), color_strategy()),
                0..8,
            ),
        ) {
            let base = palette_of(colors.try_into().expect("ten colors"));
            let effective = effective_palette(&base, &overrides);
            for role in ALL_ROLES {
                let expected = overrides
                    .iter()
                    .rev()
                    .find(|(target, _)| *target == role)
                    .map(|(_, color)| *color)
                    .unwrap_or_else(|| base.get(role));
                prop_assert_eq!(effective.get(role), expected, "role {:?}", role);
            }
        }

        /// The override pack's JSON round-trips: extends kept, roles
        /// deduped to their last write, ordered by role spelling.
        #[test]
        fn prop_override_json_round_trips(
            name in "[a-z ]{0,10}",
            extends in "[a-z]{0,8}",
            roles in proptest::collection::vec(
                (proptest::sample::select(ALL_ROLES.to_vec()), color_strategy()),
                0..ALL_ROLES.len(),
            ),
        ) {
            let json = override_to_json(&name, &extends, &roles);
            let parsed = parse_override(&json).expect("own output parses");
            prop_assert_eq!(parsed.extends, extends);
            let mut map = std::collections::BTreeMap::new();
            for (role, color) in roles {
                map.insert(role, color);
            }
            let mut expected: Vec<(Role, ColorValue)> = map.into_iter().collect();
            expected.sort_by_key(|(role, _)| role_name(*role));
            prop_assert_eq!(parsed.roles, expected);
        }

        /// from_pairs accepts the roles in any order and refuses a hole.
        #[test]
        fn prop_from_pairs_needs_every_role(
            colors in proptest::collection::vec(color_strategy(), ALL_ROLES.len()),
            rotate in 0..ALL_ROLES.len(),
            drop_at in 0..ALL_ROLES.len(),
        ) {
            let mut pairs: Vec<(Role, ColorValue)> = ALL_ROLES.into_iter().zip(colors).collect();
            pairs.rotate_left(rotate);
            let palette = Palette::from_pairs(&pairs).expect("all roles present");
            for (role, color) in &pairs {
                prop_assert_eq!(palette.get(*role), *color);
            }
            let mut hole = pairs.clone();
            hole.remove(drop_at);
            prop_assert!(matches!(
                Palette::from_pairs(&hole),
                Err(ThemeError::MissingRoles(_))
            ));
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    #[test]
    fn contrast_anchors_pin_the_xterm_palette() {
        let black = ColorValue::Indexed(0);
        assert!(
            (contrast_ratio(ColorValue::Indexed(231), black) - 21.0).abs() < 0.01,
            "index 231 is the cube's white"
        );
        assert!(
            (contrast_ratio(ColorValue::Indexed(232), black) - 1.05).abs() < 0.01,
            "index 232 starts the gray ramp at v=8"
        );
        assert!(
            (contrast_ratio(ColorValue::Indexed(255), black) - 18.10).abs() < 0.05,
            "index 255 tops the gray ramp at v=238"
        );
        assert!(
            (contrast_ratio(ColorValue::Indexed(56), black) - 2.47).abs() < 0.02,
            "index 56 is a second-layer cube cell (95,0,215)"
        );
        assert!(
            (contrast_ratio(ColorValue::Indexed(8), black) - 5.32).abs() < 0.02,
            "index 8 is mid gray"
        );
        assert!(
            (contrast_ratio(ColorValue::Indexed(21), black) - 2.44).abs() < 0.02,
            "index 21 is the cube's pure blue"
        );
    }
}
