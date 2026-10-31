//! The first-run welcome (Part10 §10.2): a document in a window, not a
//! modal — the desktop is usable behind it. The fixed styled-text builder
//! names the three things AGony does (start menu, command bar, file
//! panel) with the user's actual keys for each, states the font
//! decision already made at startup, and says how it reopens. Skipped
//! with `Esc` (or the window's close button), remembered by a marker
//! file in the config dir — once skipped it never returns uninvited —
//! and reopenable from the start menu's SETTINGS section ("About AGony").
//!
//! Items: `Content` — the built lines (styled through the applier at
//! build time), drawn once per frame.
//! Core functions: `build` — the one text builder; `rect` — the
//! centered window; `render` — the paragraph; `seen` / `mark_seen` —
//! the marker file (the `_in` variants are the tests' seam).
//! Depends on: crate::settings (paths, Combos), agony_core::inputmap
//! (the combo spelling), agony_core::thememodel (roles), theme.
//! Called by: app (open/close/draw), wm (render).
//! Calls: std::fs for the marker only.

use std::path::{Path, PathBuf};

use agony_core::config::FontMode;
use agony_core::inputmap;
use agony_core::layout::Rect;
use agony_core::thememodel::Role;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::settings::{self, Combos};
use crate::theme::Applier;

const MARKER_FILE: &str = "welcome.done";

/// The welcome window's built content.
#[derive(Clone, Debug, Default)]
pub struct Content {
    pub lines: Vec<Line<'static>>,
}

/// The marker file: `<config>/agony/welcome.done`.
pub fn marker_path() -> PathBuf {
    settings::config_dir().join(MARKER_FILE)
}

/// Whether the welcome was already dismissed once.
pub fn seen() -> bool {
    seen_in(&settings::config_dir())
}

fn seen_in(config_dir: &Path) -> bool {
    config_dir.join(MARKER_FILE).exists()
}

/// Remember the dismissal — never returns uninvited again.
pub fn mark_seen() {
    let _ = std::fs::write(marker_path(), b"");
}

/// The welcome's window: a fixed-size document, centered.
pub fn rect(desktop: Rect) -> Rect {
    let width = 64.min(desktop.width);
    let height = 15.min(desktop.height);
    let x = desktop.x + desktop.width.saturating_sub(width) / 2;
    let y = desktop.y + desktop.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

/// The one text builder (10.2.1): the three things and their keys — the
/// user's actual combos, not assumed defaults — plus the font decision
/// the startup probe already made, and the reopen path. Every color
/// resolves through the applier here, once, at build time.
pub fn build(font: FontMode, font_note: Option<&str>, combos: &Combos, theme: &Applier) -> Content {
    let text = |role| theme.style(role, Role::Background);
    let key = theme
        .style(Role::Accent, Role::Background)
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![Line::from("")];
    lines.push(
        Line::from(Span::styled(
            "Welcome to AGony",
            text(Role::Text).add_modifier(Modifier::BOLD),
        ))
        .alignment(ratatui::layout::Alignment::Center),
    );
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "A desktop that lives in your terminal. Three things to try:",
        text(Role::TextDim),
    )));
    lines.push(Line::from(""));
    lines.push(row(
        "Start menu",
        &pretty(&combo(combos.start_menu)),
        "apps, places, settings",
        &text,
        &key,
    ));
    lines.push(row(
        "Command bar",
        &pretty(&combo(combos.bar)),
        "type a command; it suggests the rest",
        &text,
        &key,
    ));
    lines.push(row(
        "File panel",
        "docked",
        "arrows walk it, / searches, g grabs a file",
        &text,
        &key,
    ));
    lines.push(Line::from(""));
    lines.push(font_line(font, font_note, &text));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "THANK-YOU.md sits on the desktop — every dependency, credited.",
        text(Role::TextDim),
    )));
    lines.push(Line::from(Span::styled(
        "This window reopens from the menu: SETTINGS > About AGony.",
        text(Role::TextDim),
    )));
    Content { lines }
}

fn row(
    label: &str,
    key: &str,
    hint: &str,
    text: &dyn Fn(Role) -> ratatui::style::Style,
    key_style: &ratatui::style::Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<14}"), text(Role::Text)),
        Span::styled(format!("{key:<11}"), *key_style),
        Span::styled(hint.to_string(), text(Role::TextDim)),
    ])
}

fn font_line(
    font: FontMode,
    note: Option<&str>,
    text: &dyn Fn(Role) -> ratatui::style::Style,
) -> Line<'static> {
    let decided = match font {
        FontMode::Nerd => "Font: nerd mode — icons and glyph buttons are on.",
        FontMode::Ascii | FontMode::Auto => "Font: ascii mode — safe everywhere, no icon glyphs.",
    };
    let note = note.unwrap_or("");
    Line::from(vec![
        Span::styled("  ", text(Role::Text)),
        Span::styled(decided.to_string(), text(Role::Text)),
        Span::styled(note.to_string(), text(Role::TextDim)),
    ])
}

fn combo(combo: inputmap::Combo) -> String {
    inputmap::combo_to_str(combo)
}

/// "ctrl-space" reads "Ctrl-Space" in the document.
fn pretty(spec: &str) -> String {
    spec.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join("-")
}

/// Draw the document (the wm routes internal windows here).
pub fn render(frame: &mut Frame<'_>, inner: Rect, content: &Content) {
    frame.render_widget(Paragraph::new(content.lines.clone()), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::buffer_text;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn combos() -> Combos {
        settings::reserved_combos(&agony_core::config::Settings::default()).0
    }

    fn content_text(content: &Content) -> String {
        content
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.clone()))
            .collect()
    }

    #[test]
    fn build_names_the_three_things_with_real_keys() {
        let theme = Applier::fallback();
        let content = build(FontMode::Ascii, None, &combos(), &theme);
        let text = content_text(&content);
        for expected in [
            "Welcome to AGony",
            "Start menu",
            "Command bar",
            "File panel",
        ] {
            assert!(text.contains(expected), "{expected} missing: {text}");
        }
        assert!(
            text.contains("Ctrl-Space"),
            "the start menu's default key shows: {text}"
        );
        assert!(
            text.contains("Ctrl-K"),
            "the bar's default key shows: {text}"
        );
        assert!(
            text.contains("ascii mode"),
            "the font decision is stated: {text}"
        );
        assert!(
            text.contains("About AGony"),
            "the reopen path is named: {text}"
        );
        assert!(text.contains("THANK-YOU.md"), "the desktop document named");
    }

    #[test]
    fn nerd_mode_reads_as_decided() {
        let theme = Applier::fallback();
        let text = content_text(&build(FontMode::Nerd, None, &combos(), &theme));
        assert!(text.contains("nerd mode"), "{text}");
    }

    #[test]
    fn a_custom_combo_shows_not_the_default() {
        let settings = agony_core::config::Settings {
            reserved_keys: agony_core::config::ReservedKeys {
                bar: "ctrl-b".into(),
                ..agony_core::config::ReservedKeys::default()
            },
            ..agony_core::config::Settings::default()
        };
        let (combos, _) = settings::reserved_combos(&settings);
        let theme = Applier::fallback();
        let text = content_text(&build(FontMode::Ascii, None, &combos, &theme));
        assert!(text.contains("Ctrl-B"), "the user's bar key shows: {text}");
    }

    #[test]
    fn pretty_capitalizes_each_part() {
        assert_eq!(pretty("ctrl-space"), "Ctrl-Space");
        assert_eq!(pretty("ctrl-tab"), "Ctrl-Tab");
        assert_eq!(pretty("ctrl"), "Ctrl");
    }

    #[test]
    fn rect_centers_a_fixed_document() {
        let desktop = Rect::new(0, 0, 100, 30);
        assert_eq!(rect(desktop), Rect::new(18, 7, 64, 15));
        let small = Rect::new(0, 0, 40, 10);
        assert_eq!(
            rect(small),
            Rect::new(0, 0, 40, 10),
            "clamped inside the desktop"
        );
    }

    #[test]
    fn the_marker_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("agony-welcome-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        assert!(!seen_in(&dir), "fresh config: not seen");
        std::fs::write(dir.join(MARKER_FILE), b"").expect("marker written");
        assert!(seen_in(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drawn_content_shows_the_document() {
        let theme = Applier::fallback();
        let content = build(FontMode::Ascii, None, &combos(), &theme);
        let backend = TestBackend::new(64, 15);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| render(f, Rect::new(1, 1, 62, 13), &content))
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("Welcome to AGony"), "{text:?}");
        assert!(text.contains("Ctrl-Space"), "{text:?}");
        assert!(text.contains("THANK-YOU.md"), "{text:?}");
    }
}
