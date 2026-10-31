//! The taskbar: start button on the left, one button per open window
//! (Part04 4.6), a transient status line (or the window-keys hint) in
//! the middle, the injected clock on the right. Hover (Part06 6.3.5)
//! highlights the hovered button through the theme — it never moves
//! focus.
//!
//! Items: `View` — everything one draw needs (theme, font, clock, status
//! or hint, menu flag, the pointer cell, the window list); `TaskWindow`
//! — one window's taskbar face.
//! Core functions: `draw` — one row from a `View` (the clock is a string
//! the caller computed; nothing here reads time); `start_button_rect` —
//! the start hit-test rectangle; `window_button_rects` — the window
//! buttons' hit-test rectangles; `prompt_rect` — the middle zone (the
//! command bar's click-to-open target, Part07). All rect functions
//! derive from the same widths `draw` uses, so a click lands where the
//! button is drawn.
//! Depends on: theme (colors), agony_core::icon (the start glyph),
//! agony_core::config (FontMode), agony_core::layout (Rect).
//! Called by: app's draw pass and mouse handler.
//! Calls: nothing.

use agony_core::config::FontMode;
use agony_core::icon::{self as icon_table, Icon};
use agony_core::layout::Rect;
use ratatui::layout::Alignment;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme::{Applier, Role};

/// One open window as the taskbar shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskWindow {
    pub id: u64,
    pub title: String,
    pub active: bool,
    pub minimized: bool,
}

pub struct View<'a> {
    pub theme: &'a Applier,
    pub font: FontMode,
    pub clock: &'a str,
    pub status: Option<&'a str>,
    /// The window-keys hint, shown when there is no status to report
    /// (Part04 4.5.1's documented-in-the-taskbar rule).
    pub hint: &'a str,
    pub menu_open: bool,
    /// The pointer's cell, for button hover styling (None = away).
    pub hover: Option<(u16, u16)>,
    pub windows: &'a [TaskWindow],
}

/// The longest window-button label before truncation.
const MAX_BUTTON_LABEL: usize = 12;

pub fn draw(frame: &mut Frame<'_>, region: Rect, view: &View<'_>) {
    let bar = Paragraph::new("").style(view.theme.style(Role::Text, Role::Surface));
    frame.render_widget(bar, region);
    let label = start_label(view.font);
    let mut x = region.x + label.chars().count() as u16 + 2;
    for button in view.windows {
        let width = button_width(button);
        let rect = Rect::new(x, region.y, width.min(region.right().saturating_sub(x)), 1);
        let hovered = point_in_rect(view.hover, rect);
        frame.render_widget(button_paragraph(button, view, hovered), rect);
        x = x.saturating_add(width);
    }
    let start_width = (label.chars().count() as u16 + 2).min(region.width);
    let start_rect = Rect::new(region.x, region.y, start_width, 1);
    let hovered = point_in_rect(view.hover, start_rect);
    frame.render_widget(start_button(&label, view, hovered), start_rect);
    let clock_x = region
        .right()
        .saturating_sub(view.clock.chars().count() as u16 + 2);
    // The status/hint zone starts PAST the last window button — the same
    // origin `prompt_rect` hit-tests with — so a status line can never
    // paint over a button (the UI review's overwrite finding).
    let after_buttons = x.max(region.x + start_width);
    let middle_end = clock_x.max(after_buttons);
    let middle = Rect::new(
        after_buttons,
        region.y,
        middle_end.saturating_sub(after_buttons),
        1,
    );
    frame.render_widget(status_line(view, middle.width), middle);
    frame.render_widget(
        clock_line(view),
        Rect::new(clock_x, region.y, region.right().saturating_sub(clock_x), 1),
    );
}

/// The middle zone between the last window button and the clock's
/// reserved cells — the status/hint line's home and the command bar's
/// click-to-open area (Part07 7.2.1). It starts past the window
/// buttons, and the clock reserve is the clock's widest shape (`HH:MM`
/// plus padding), so the zone never swallows a click meant for either.
pub fn prompt_rect(region: Rect, font: FontMode, windows: &[TaskWindow]) -> Rect {
    const CLOCK_RESERVE: u16 = 8;
    let start = start_button_rect(region, font);
    let mut after_buttons = start.right();
    for button in window_button_rects(region, font, windows) {
        after_buttons = after_buttons.max(button.1.right());
    }
    let clock_x = region.right().saturating_sub(CLOCK_RESERVE);
    let middle_end = clock_x.max(after_buttons);
    Rect::new(
        after_buttons,
        region.y,
        middle_end.saturating_sub(after_buttons),
        1,
    )
}

fn point_in_rect(point: Option<(u16, u16)>, rect: Rect) -> bool {
    let Some((col, row)) = point else {
        return false;
    };
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// The hit-test rectangle for the start button — the same width `draw`
/// gives it, so a click lands where the button is drawn.
pub fn start_button_rect(region: Rect, font: FontMode) -> Rect {
    let width = (start_label(font).chars().count() as u16 + 2).min(region.width);
    Rect::new(region.x, region.y, width, 1)
}

/// The hit-test rectangles for the window buttons, in list order — the
/// same widths `draw` gives them.
pub fn window_button_rects(
    region: Rect,
    font: FontMode,
    windows: &[TaskWindow],
) -> Vec<(u64, Rect)> {
    let start = start_button_rect(region, font);
    let mut x = start.right();
    windows
        .iter()
        .map(|button| {
            let width = button_width(button).min(region.right().saturating_sub(x));
            let rect = Rect::new(x, region.y, width, 1);
            x = x.saturating_add(button_width(button));
            (button.id, rect)
        })
        .collect()
}

fn button_width(button: &TaskWindow) -> u16 {
    agony_core::textutil::ellipsize(&button.title, MAX_BUTTON_LABEL)
        .chars()
        .count() as u16
        + 3
}

fn start_label(font: FontMode) -> String {
    let glyph = icon_table::glyph(Icon::Start, font);
    if font == FontMode::Nerd {
        format!("{glyph} Start")
    } else {
        glyph.to_string()
    }
}

fn start_button(label: &str, view: &View<'_>, hovered: bool) -> Paragraph<'static> {
    let role = if view.menu_open || hovered {
        Role::Accent
    } else {
        Role::Text
    };
    let mut style = view.theme.style(role, Role::Surface);
    if view.menu_open {
        style = style.add_modifier(Modifier::BOLD);
    }
    Paragraph::new(Line::from(format!(" {label} "))).style(style)
}

/// A window button: the title (bracketed when minimized — the pressed
/// state), accent when active, accent-when-hovered otherwise (Part06
/// 6.3.5 — a highlight, never a focus change).
fn button_paragraph(button: &TaskWindow, view: &View<'_>, hovered: bool) -> Paragraph<'static> {
    let shown = agony_core::textutil::ellipsize(&button.title, MAX_BUTTON_LABEL);
    let label = if button.minimized {
        format!("[{shown}]")
    } else {
        format!(" {shown} ")
    };
    Paragraph::new(Line::from(label)).style(button_style(button, view, hovered))
}

/// The button's style decision (separate from the paragraph so tests
/// can pin the hover highlight). The active button wears the Selection
/// role — the taskbar's "this is the selected task" treatment, distinct
/// from the hover accent (the UI review's role-split finding).
fn button_style(button: &TaskWindow, view: &View<'_>, hovered: bool) -> ratatui::style::Style {
    if button.active {
        view.theme
            .style(Role::Background, Role::Selection)
            .add_modifier(Modifier::BOLD)
    } else if hovered {
        view.theme.style(Role::Accent, Role::Surface)
    } else {
        view.theme.style(Role::Text, Role::Surface)
    }
}

/// The status/hint line, cut to the zone it actually has (`width`) —
/// never spilling over a neighbor.
fn status_line(view: &View<'_>, width: u16) -> Paragraph<'static> {
    let text = view.status.unwrap_or(view.hint).to_string();
    let text = agony_core::textutil::ellipsize(&text, width as usize);
    Paragraph::new(text)
        .style(view.theme.style(Role::TextDim, Role::Surface))
        .alignment(Alignment::Center)
}

fn clock_line(view: &View<'_>) -> Paragraph<'static> {
    let line = Line::from(Span::styled(
        format!(" {} ", view.clock),
        view.theme.style(Role::Text, Role::Surface),
    ));
    Paragraph::new(line).style(view.theme.style(Role::Text, Role::Surface))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::buffer_text;
    use ratatui::{backend::TestBackend, Terminal};

    fn window(id: u64, title: &str, active: bool, minimized: bool) -> TaskWindow {
        TaskWindow {
            id,
            title: title.into(),
            active,
            minimized,
        }
    }

    fn rendered(view: &View<'_>) -> String {
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| draw(f, Rect::new(0, 0, 80, 1), view))
            .expect("draw");
        buffer_text(&terminal)
    }

    #[test]
    fn taskbar_shows_start_clock_status_and_windows() {
        let theme = Applier::fallback();
        let windows = vec![
            window(1, "PowerShell", true, false),
            window(2, "Glow", false, true),
        ];
        let view = View {
            theme: &theme,
            font: FontMode::Ascii,
            clock: "12:34",
            status: Some("hello"),
            hint: "^E focus",
            menu_open: false,
            hover: None,
            windows: &windows,
        };
        let text = rendered(&view);
        assert!(text.contains("Start"), "start button missing: {text:?}");
        assert!(text.contains("12:34"), "clock missing: {text:?}");
        assert!(text.contains("hello"), "status missing: {text:?}");
        assert!(
            text.contains("PowerShell"),
            "window button missing: {text:?}"
        );
        assert!(
            text.contains("[Glow]"),
            "minimized bracket missing: {text:?}"
        );
    }

    #[test]
    fn hint_shows_when_there_is_no_status() {
        let theme = Applier::fallback();
        let view = View {
            theme: &theme,
            font: FontMode::Ascii,
            clock: "00:00",
            status: None,
            hint: "^E focus · ^W close",
            menu_open: false,
            hover: None,
            windows: &[],
        };
        assert!(rendered(&view).contains("^E focus"));
    }

    #[test]
    fn taskbar_ascii_mode_has_no_nerd_codepoints() {
        let theme = Applier::fallback();
        let view = View {
            theme: &theme,
            font: FontMode::Ascii,
            clock: "00:00",
            status: None,
            hint: "",
            menu_open: false,
            hover: None,
            windows: &[],
        };
        assert!(rendered(&view).is_ascii());
    }

    #[test]
    fn taskbar_nerd_mode_carries_the_start_glyph() {
        let theme = Applier::fallback();
        let view = View {
            theme: &theme,
            font: FontMode::Nerd,
            clock: "00:00",
            status: None,
            hint: "",
            menu_open: false,
            hover: None,
            windows: &[],
        };
        assert!(
            rendered(&view).contains('\u{f17b}'),
            "nerd start glyph missing"
        );
    }

    #[test]
    fn start_button_rect_matches_the_drawn_label() {
        let region = Rect::new(0, 24, 80, 1);
        let ascii = start_button_rect(region, FontMode::Ascii);
        let nerd = start_button_rect(region, FontMode::Nerd);
        assert_eq!(ascii.width, "Start".chars().count() as u16 + 2);
        assert!(nerd.width > ascii.width);
        assert_eq!(ascii.y, 24);
    }

    #[test]
    fn window_button_rects_sit_after_the_start_button_in_order() {
        let theme = Applier::fallback();
        let windows = vec![window(1, "abc", true, false), window(2, "de", false, false)];
        let view = View {
            theme: &theme,
            font: FontMode::Ascii,
            clock: "12:00",
            status: None,
            hint: "",
            menu_open: false,
            hover: None,
            windows: &windows,
        };
        let region = Rect::new(0, 24, 80, 1);
        let rects = window_button_rects(region, FontMode::Ascii, &windows);
        let first = start_button_rect(region, FontMode::Ascii);
        assert_eq!(rects[0].0, 1);
        assert_eq!(rects[0].1.x, first.right(), "first button after start");
        assert_eq!(rects[1].1.x, rects[0].1.right());
        assert_eq!(rects[0].1.width, "abc".chars().count() as u16 + 3);
        let _ = &view;
    }

    #[test]
    fn prompt_rect_leaves_the_buttons_and_the_clock_their_cells() {
        let region = Rect::new(0, 24, 80, 1);
        let windows = [
            window(1, "abc", false, false),
            window(2, "de", false, false),
        ];
        let prompt = prompt_rect(region, FontMode::Ascii, &windows);
        let last = window_button_rects(region, FontMode::Ascii, &windows);
        assert_eq!(
            prompt.x,
            last.last().map(|(_, rect)| rect.right()).unwrap_or(0),
            "the prompt starts past the last window button"
        );
        assert!(
            prompt.right() <= region.right() - 7,
            "the clock keeps its reserve"
        );
    }

    #[test]
    fn a_status_line_never_paints_over_a_window_button() {
        // The regression behind the review's "pinned error over the
        // buttons": the status zone starts past the last window button.
        let theme = Applier::fallback();
        let windows = vec![window(1, "abc", false, false)];
        let view = View {
            theme: &theme,
            font: FontMode::Ascii,
            clock: "00:00",
            status: Some(
                "catalog remote (cached) ignored: manifest parse failed: TOML parse error",
            ),
            hint: "",
            menu_open: false,
            hover: None,
            windows: &windows,
        };
        let text = rendered(&view);
        let button = window_button_rects(Rect::new(0, 0, 80, 1), FontMode::Ascii, &windows)[0].1;
        let cells: Vec<char> = text.chars().collect();
        let under: String = cells
            .iter()
            .skip(button.x as usize)
            .take(button.width as usize)
            .collect();
        assert!(
            under.contains("abc"),
            "the button's cells stay the button's: {under:?}"
        );
    }

    #[test]
    fn hover_restyles_an_idle_window_button() {
        let theme = Applier::fallback();
        let idle = window(2, "abc", false, false);
        let windows = [idle.clone()];
        let view = View {
            theme: &theme,
            font: FontMode::Ascii,
            clock: "00:00",
            status: None,
            hint: "",
            menu_open: false,
            hover: None,
            windows: &windows,
        };
        let plain = button_style(&idle, &view, false);
        let hovered = button_style(&idle, &view, true);
        assert_ne!(
            plain.fg, hovered.fg,
            "hover highlights through the applier, not a literal"
        );
    }
}
