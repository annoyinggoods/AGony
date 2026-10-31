//! The paste menu (Part07 7.5): the three forms of a dropped file —
//! name, absolute path, relative to the bar's context — each row
//! showing the exact text it will insert, the preview following the
//! choice. The forms are `pathpick`'s alone (`pick_text` is the one
//! producer); nothing here computes a path.
//!
//! Items: `Menu` — the open menu's state (the chosen form's index);
//! `Form` — one choice (label, preview).
//! Core functions: `Menu::move_selection` — Up/Down; `forms` — the
//! three choices with their exact insertion previews; `chosen_text` —
//! the text the chosen form inserts (None without a picked file);
//! `draw` — the widget.
//! Depends on: agony_core::pathpick (the only text producer), theme.
//! Called by: commandbar (open/move/insert) and app (the draw pass).
//! Calls: nothing.

use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use agony_core::pathpick::{self, PickForm};

use crate::theme::{Applier, Role};

pub const FORM_COUNT: usize = 3;

/// The open paste menu: which form is chosen (an index into
/// [`forms`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Menu {
    pub chosen: usize,
}

impl Menu {
    pub fn new() -> Menu {
        Menu { chosen: 0 }
    }

    /// Move the choice, clamped at the ends (Up/Down are the same
    /// arrows the bar's list uses).
    pub fn move_selection(&mut self, delta: i32) {
        let last = FORM_COUNT.saturating_sub(1) as i32;
        let moved = self.chosen as i32 + delta;
        self.chosen = moved.clamp(0, last) as usize;
    }
}

/// One choice: its name and the exact text Enter would insert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub label: String,
    pub preview: String,
    pub form: PickForm,
}

/// The three forms of `picked` against the bar's context cwd —
/// `pathpick::pick_text` produces every preview, so what is shown is
/// what is inserted (Part07 7.5.1's exact-preview rule).
pub fn forms(picked: &str, cwd: &str) -> Vec<Form> {
    let specs: [(String, PickForm); FORM_COUNT] = [
        ("name".to_string(), PickForm::Name),
        ("absolute path".to_string(), PickForm::Absolute),
        (format!("relative to {cwd}"), PickForm::Relative),
    ];
    specs
        .into_iter()
        .map(|(label, form)| {
            let preview = pathpick::pick_text(picked, cwd, form);
            Form {
                label,
                preview,
                form,
            }
        })
        .collect()
}

/// The insertion text of the chosen form, placed at the cursor by the
/// bar (Part07 7.5.2 — quoting included, `pathpick`'s decision).
pub fn chosen_text(picked: &str, cwd: &str, menu: &Menu) -> String {
    let forms = forms(picked, cwd);
    forms
        .get(menu.chosen)
        .map(|form| form.preview.clone())
        .unwrap_or_default()
}

/// The rows the menu renders (the bar's hit-test reads this too): one
/// line per form, then the hint line.
pub fn lines(menu: &Menu, picked: &str, cwd: &str, theme: &Applier) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (at, form) in forms(picked, cwd).into_iter().enumerate() {
        let chosen = at == menu.chosen;
        let style = if chosen {
            theme.style(Role::Background, Role::Accent)
        } else {
            theme.style(Role::Text, Role::Background)
        };
        let dim = theme.style(Role::TextDim, Role::Background);
        lines.push(Line::from(vec![
            Span::styled(if chosen { "▸ " } else { "  " }.to_string(), style),
            Span::styled(format!("{:<14}", form.label), style),
            Span::styled(form.preview, dim),
        ]));
    }
    lines.push(
        Line::from("Enter insert · Esc cancel").style(theme.style(Role::TextDim, Role::Background)),
    );
    lines
}

pub fn draw(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    menu: &Menu,
    picked: &str,
    cwd: &str,
    theme: &Applier,
) {
    frame.render_widget(Paragraph::new(lines(menu, picked, cwd, theme)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_forms_show_their_exact_previews() {
        let forms = forms(r"C:\docs\my report.md", r"C:\work");
        assert_eq!(forms.len(), 3);
        assert_eq!(forms[0].label, "name");
        assert_eq!(
            forms[0].preview, "\"my report.md\"",
            "quoting is pathpick's"
        );
        assert_eq!(forms[1].label, "absolute path");
        assert_eq!(forms[1].preview, "\"C:\\docs\\my report.md\"");
        assert_eq!(forms[2].label, "relative to C:\\work");
        assert_eq!(
            forms[2].preview, r#""..\docs\my report.md""#,
            "the space quotes — pathpick's call, shown exactly"
        );
    }

    #[test]
    fn chosen_text_follows_the_choice() {
        let picked = r"C:\docs\report.md";
        let mut menu = Menu::new();
        assert_eq!(chosen_text(picked, r"C:\work", &menu), "report.md");
        menu.move_selection(1);
        assert_eq!(chosen_text(picked, r"C:\work", &menu), r"C:\docs\report.md");
        menu.move_selection(1);
        assert_eq!(chosen_text(picked, r"C:\work", &menu), r"..\docs\report.md");
        menu.move_selection(1);
        assert_eq!(menu.chosen, 2, "clamped at the last form");
        menu.move_selection(-5);
        assert_eq!(menu.chosen, 0, "clamped at the first form");
    }

    #[test]
    fn previews_update_without_recomputing_the_menu() {
        // The preview in `forms` IS the insertion text: one producer
        // (pathpick::pick_text), the menu only indexes it.
        let forms = forms("plain.txt", r"C:\w");
        let mut menu = Menu::new();
        menu.move_selection(2);
        let chosen = chosen_text("plain.txt", r"C:\w", &menu);
        assert_eq!(chosen, forms[2].preview);
    }
}
