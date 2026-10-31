//! The window chrome renderer (Part04 4.3): every visible window painted
//! bottom of the z-order up, so higher windows cover lower ones — the
//! desktop metaphor, not tiling. The focused window's border and titlebar
//! take the theme's focus/titlebar-active roles; the others idle. A
//! hovered titlebar (Part06 6.3.5) brightens its text — hover highlights,
//! it never moves focus.
//!
//! Titles come from the window model (the manifest's name), never from a
//! pane title — the clobbered-title rule.
//! Items: `Buttons` — the titlebar glyphs (nerd/ascii forms together);
//! `View` — one draw's inputs (theme, font, the pointer cell for hover,
//! the panel view when the window is internal).
//! Core functions: `draw` — the whole stack; `chrome` — one window's
//! border + titlebar (shared by the panel's window); `titlebar_line` —
//! the title + buttons row, pure text (tests pin it).
//! Depends on: agony_core::layout (geometry), agony_core::icon (folder
//! marker), theme (roles), ratatui-rmux (the pane widget — render-only
//! types), panel (the file panel's content), settings_ui (the settings
//! window's content), welcome (the welcome document), directory (the
//! app directory window).
//! Called by: app's draw pass.
//! Calls: nothing async; drivers' snapshots are already refreshed.

use agony_core::config::FontMode;
use agony_core::icon::{self as icon_table, Icon};
use agony_core::layout::{effective_rect, WmState, TITLE_BUTTON_COLS};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use ratatui_rmux::PaneWidget;

use crate::directory;
use crate::panel;
use crate::reconcile::Panes;
use crate::settings_ui;
use crate::theme::{Applier, Role};
use crate::welcome;

/// One draw's inputs beyond the state itself.
pub struct View<'a> {
    pub theme: &'a Applier,
    pub font: FontMode,
    /// The pointer's cell, for titlebar hover styling (None = away).
    pub hover: Option<(u16, u16)>,
    /// The internal file-panel window's content, when it is open.
    pub panel: Option<panel::View<'a>>,
    /// The settings window: its id and its content (an internal window,
    /// Part08).
    pub settings: Option<(u64, settings_ui::View<'a>)>,
    /// The welcome document: its id and its built lines (Part10).
    pub welcome: Option<(u64, &'a welcome::Content)>,
    /// The app directory window: its id and its state (Part11).
    pub directory: Option<(u64, &'a mut directory::State)>,
}

/// The whole stack: background windows first, focused last, each pane
/// snapshot painted into its own window's inner rect. An overlapped pane
/// keeps its size — the covering window simply paints over it.
pub fn draw(frame: &mut Frame<'_>, wm: &WmState, panes: &Panes, view: &mut View<'_>) {
    let order: Vec<u64> = wm.visible_windows();
    for id in order.iter().rev() {
        draw_window(frame, wm, *id, panes, view);
    }
}

fn draw_window(frame: &mut Frame<'_>, wm: &WmState, id: u64, panes: &Panes, view: &mut View<'_>) {
    let Some(window) = wm.window(id) else {
        return;
    };
    let focused = wm.focus == Some(id);
    let rect = effective_rect(window, wm.desktop);
    // The window owns every cell of its rect: the Clear blanks whatever
    // the windows below left there (the UI review's bleed-through
    // finding), then chrome and content paint over blank cells.
    frame.render_widget(Clear, rect);
    let hovered = titlebar_hovered(rect, view.hover);
    let inner = chrome(
        frame,
        rect,
        &window.title,
        focused,
        hovered,
        view.theme,
        view.font,
    );
    if window.pane_id.is_some() {
        draw_pane(frame, id, panes, inner, view);
        return;
    }
    draw_internal(frame, id, inner, focused, view);
}

fn draw_pane(
    frame: &mut Frame<'_>,
    id: u64,
    panes: &Panes,
    inner: agony_core::layout::Rect,
    view: &mut View<'_>,
) {
    if let Some(driver) = panes.driver(id) {
        let widget = PaneWidget::new(driver.state())
            .base_style(view.theme.style(Role::Text, Role::Background));
        frame.render_widget(widget, inner);
    }
}

/// The internal windows (no pane): the file panel, the settings window,
/// the welcome document and the app directory — each renders only into
/// its own window, matched by id. The panel is the catch-all ("whatever
/// internal window is left"), because it is the only one without a
/// carried id in the view.
fn draw_internal(
    frame: &mut Frame<'_>,
    id: u64,
    inner: agony_core::layout::Rect,
    focused: bool,
    view: &mut View<'_>,
) {
    if let Some((window_id, settings_view)) = view.settings.as_mut() {
        if *window_id == id {
            settings_ui::render(frame, inner, settings_view);
            return;
        }
    }
    if let Some((window_id, content)) = view.welcome.as_ref() {
        if *window_id == id {
            welcome::render(frame, inner, content);
            return;
        }
    }
    if let Some((window_id, state)) = view.directory.as_mut() {
        if *window_id == id {
            directory::render_window(frame, inner, state, view.theme);
            return;
        }
    }
    if let Some(panel_view) = view.panel.as_mut() {
        panel::render(frame, inner, panel_view, focused, view.theme, view.font);
    }
}

/// Whether the pointer rests anywhere on this window's titlebar row.
fn titlebar_hovered(rect: agony_core::layout::Rect, hover: Option<(u16, u16)>) -> bool {
    let Some((col, row)) = hover else {
        return false;
    };
    row == rect.y && col >= rect.x && col < rect.x + rect.width
}

/// One window's border and titlebar; returns the inner rect (where the
/// pane or the panel's content goes). The titlebar is the border's top
/// row: title left, buttons right (`TITLE_BUTTON_COLS` cells — the same
/// zone `hit_test` classifies). `hovered` brightens the title text of an
/// unfocused window.
pub fn chrome(
    frame: &mut Frame<'_>,
    rect: agony_core::layout::Rect,
    title: &str,
    focused: bool,
    hovered: bool,
    theme: &Applier,
    font: FontMode,
) -> agony_core::layout::Rect {
    let (border_role, bar_bg) = if focused {
        (Role::Focus, Role::TitlebarActive)
    } else {
        (Role::TitlebarIdle, Role::TitlebarIdle)
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.style(border_role, border_role))
        .style(theme.style(Role::Text, Role::Background));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let bar = titlebar_area(rect);
    frame.render_widget(
        Paragraph::new(titlebar_line(
            title, bar.width, focused, hovered, theme, font,
        ))
        .style(theme.style(Role::Text, bar_bg)),
        bar,
    );
    inner
}

fn titlebar_area(rect: agony_core::layout::Rect) -> agony_core::layout::Rect {
    let width = rect.width.saturating_sub(2).max(1);
    agony_core::layout::Rect::new(rect.x + 1, rect.y, width, 1)
}

/// The titlebar as pure text, `bar_width` cells wide: ` title` left, the
/// three buttons right-aligned in the `TITLE_BUTTON_COLS` zone — the same
/// zone `hit_test` classifies. A long title truncates; it never pushes
/// the buttons out of the zone. Hover (Part06) reads a focused bar's
/// text and an idle bar's dim text at full brightness.
pub fn titlebar_line<'a>(
    title: &str,
    bar_width: u16,
    focused: bool,
    hovered: bool,
    theme: &Applier,
    font: FontMode,
) -> Line<'a> {
    let buttons = format!(
        "{:>width$}",
        buttons_label(font),
        width = TITLE_BUTTON_COLS as usize
    );
    let title_budget = bar_width.saturating_sub(TITLE_BUTTON_COLS + 1);
    let shown = agony_core::textutil::ellipsize(title, title_budget as usize);
    let pad = bar_width
        .saturating_sub(1)
        .saturating_sub(shown.chars().count() as u16)
        .saturating_sub(TITLE_BUTTON_COLS);
    let text_role = if focused || hovered {
        Role::Text
    } else {
        Role::TextDim
    };
    let title_style = theme.style(text_role, Role::TitlebarActive);
    let button_style = theme
        .style(text_role, Role::TitlebarActive)
        .add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled(format!(" {shown}"), title_style),
        Span::raw(" ".repeat(pad as usize)),
        Span::styled(buttons, button_style),
    ])
}

/// The three window buttons as a label: `_ □ ✕` (nerd) / `- [ #` (ascii).
pub fn buttons_label(font: FontMode) -> String {
    if font == FontMode::Nerd {
        "_ \u{25a1} \u{2715}".to_string()
    } else {
        "- [ #".to_string()
    }
}

/// The folder marker the panel's rows use (one home for the glyph).
pub fn folder_glyph(font: FontMode) -> &'static str {
    icon_table::glyph(Icon::Folder, font)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::buffer_text;
    use agony_core::layout::Rect;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn buttons_label_matches_the_hit_zone_width() {
        for font in [FontMode::Ascii, FontMode::Nerd] {
            let label = buttons_label(font);
            let cells = label.chars().count() as u16;
            assert!(
                cells <= TITLE_BUTTON_COLS,
                "{font:?} buttons ({label:?}) overflow the {TITLE_BUTTON_COLS}-cell hit zone"
            );
        }
    }

    #[test]
    fn chrome_draws_title_buttons_and_round_border() {
        let theme = Applier::fallback();
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| {
                let inner = chrome(
                    f,
                    Rect::new(0, 0, 40, 10),
                    "PowerShell",
                    true,
                    false,
                    &theme,
                    FontMode::Ascii,
                );
                assert_eq!(inner, Rect::new(1, 1, 38, 8));
            })
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("PowerShell"), "title missing: {text:?}");
        assert!(text.contains("- [ #"), "ascii buttons missing: {text:?}");
        assert!(text.contains('╭'), "round border missing: {text:?}");
    }

    #[test]
    fn long_titles_yield_the_button_zone() {
        let theme = Applier::fallback();
        let line = titlebar_line(
            "a-very-long-window-title-that-cannot-fit-here",
            30,
            false,
            false,
            &theme,
            FontMode::Ascii,
        );
        let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 30, "the line fills the bar exactly");
        assert!(
            line.spans.last().is_some_and(|s| s.content.ends_with('#')),
            "the close button stays in the zone"
        );
    }

    #[test]
    fn short_titles_pad_out_to_the_button_zone() {
        let theme = Applier::fallback();
        let line = titlebar_line("w", 30, true, false, &theme, FontMode::Ascii);
        let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 30);
    }

    #[test]
    fn hover_brightens_an_idle_titlebars_text() {
        let theme = Applier::fallback();
        let idle = titlebar_line("w", 30, false, false, &theme, FontMode::Ascii);
        let hovered = titlebar_line("w", 30, false, true, &theme, FontMode::Ascii);
        let title_fg = |line: &Line<'_>| line.spans[0].style.fg;
        assert_ne!(
            title_fg(&idle),
            title_fg(&hovered),
            "hover restyles the title text through the applier"
        );
        let focused = titlebar_line("w", 30, true, false, &theme, FontMode::Ascii);
        assert_eq!(
            title_fg(&focused),
            title_fg(&hovered),
            "hover on an idle bar reads as bright as focus"
        );
    }

    #[test]
    fn titlebar_hover_tracks_the_row_and_the_width() {
        let rect = Rect::new(10, 5, 20, 8);
        assert!(titlebar_hovered(rect, Some((10, 5))));
        assert!(titlebar_hovered(rect, Some((29, 5))));
        assert!(
            !titlebar_hovered(rect, Some((30, 5))),
            "past the right edge"
        );
        assert!(
            !titlebar_hovered(rect, Some((15, 6))),
            "not the titlebar row"
        );
        assert!(!titlebar_hovered(rect, None));
    }

    #[test]
    fn a_window_clears_the_cells_it_owns() {
        // The regression behind the review's #1 finding: whatever was
        // drawn under a window never bleeds through its interior.
        let theme = Applier::fallback();
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| {
                let junk = Paragraph::new("x".repeat(400));
                f.render_widget(junk, Rect::new(0, 0, 40, 10));
                chrome(
                    f,
                    Rect::new(5, 2, 20, 6),
                    "w",
                    true,
                    false,
                    &theme,
                    FontMode::Ascii,
                );
            })
            .expect("draw");
        let text = buffer_text(&terminal);
        let cells: Vec<char> = text.chars().collect();
        // An interior row (well below the titlebar) must be blank
        // between the borders — no junk glyph survives the Clear.
        let interior_row = 4;
        let start = interior_row * 40 + 5 + 1;
        let interior: String = cells[start..start + 18].iter().collect();
        assert!(
            interior.chars().all(|c| c == ' '),
            "junk bled through the window: {interior:?}"
        );
    }

    #[test]
    fn folder_glyph_is_the_icon_table() {
        assert_eq!(folder_glyph(FontMode::Ascii), "dir");
        assert_ne!(folder_glyph(FontMode::Nerd), "dir");
    }
}
