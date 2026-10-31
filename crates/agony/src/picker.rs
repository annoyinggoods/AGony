//! The color picker overlay (Part08 §8.3): a hex input line, the
//! 256-color palette laid out by structure — the 16 base colors, the
//! 6×6×6 cube as six 36-column hue rows, the 24-step gray ramp — and a
//! preview strip; every change previews, Enter/double-click commits,
//! Esc cancels. The segmented layout is the UI review's readability
//! finding: the cube at 16 columns read as noise, at 36 its hue sweeps
//! are visible. The palette speaks both notations through the core
//! (`hex_of` / `parse_hex`); contrast is the caller's check.
//!
//! Items: `State` — the open picker (role, hex field, the cursor as a
//! palette index, focus, the previewed value, the refusal note);
//! `Focus` — hex line vs grid; `Event` — `Preview`, `Commit`, `Cancel`;
//! `Click` — what a click inside the picker hit.
//! Core functions: `State::open` — start from a role's current value;
//! `handle_key` — the pure modal routing (arrows move and preview,
//! Enter commits, Tab flips focus); `areas` — the one segment layout
//! the draw and the hit-test share; `draw` — the widget.
//! Depends on: agony_core::thememodel (roles, hex parse/spell), theme.
//! Called by: settings_ui (open, events, draw inside its pane).
//! Calls: nothing async — committing is settings_ui's decision.

use agony_core::thememodel::{self, ColorValue, Role};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme::{self, Applier};

/// The base-16 row's width (indices 0–15, one row).
pub const BASE_COLS: u16 = 16;
/// The cube's side: 6 blue rows of 36 hue columns (indices 16–231).
pub const CUBE_SIDE: u16 = 6;
pub const CUBE_COLS: u16 = CUBE_SIDE * CUBE_SIDE;
/// The gray ramp: 24 steps in two rows of 12 (indices 232–255).
pub const GRAY_COLS: u16 = 12;
pub const GRAY_ROWS: u16 = 2;
/// The lines above the palette (role+hex, the two preview strips).
const ROWS_ABOVE: u16 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Hex,
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Consumed,
    /// A change to preview (an arrow move, a valid hex edit).
    Preview(ColorValue),
    /// Enter (hex) or the commit action — the caller checks contrast.
    Commit(ColorValue),
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// The hex line: focus it.
    Hex,
    /// A palette cell (the color index).
    Cell(u8),
}

#[derive(Clone, Debug)]
pub struct State {
    pub role: Role,
    /// The hex input line's text (no leading `#` needed).
    pub hex: String,
    /// The grid cursor: a palette index (segment-aware movement keeps
    /// it on a drawn cell).
    pub cursor: u8,
    pub focus: Focus,
    /// The value being previewed (starts at the role's current color).
    pub current: ColorValue,
    /// The latest refusal (bad hex, or the caller's contrast refusal).
    pub error: Option<String>,
}

impl State {
    pub fn open(role: Role, current: ColorValue) -> State {
        State {
            role,
            hex: thememodel::hex_of(current),
            cursor: 0,
            focus: Focus::Grid,
            current,
            error: None,
        }
    }

    /// The color under the grid cursor.
    pub fn cursor_color(&self) -> ColorValue {
        ColorValue::Indexed(self.cursor)
    }
}

/// One palette segment: a rectangle of indices with its own origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Segment {
    Base,
    Cube,
    Gray,
}

/// The segments in draw and navigation order.
const SEGMENTS: [Segment; 3] = [Segment::Base, Segment::Cube, Segment::Gray];

impl Segment {
    fn shape(self) -> (u16, u16) {
        match self {
            Segment::Base => (BASE_COLS, 1),
            Segment::Cube => (CUBE_COLS, CUBE_SIDE),
            Segment::Gray => (GRAY_COLS, GRAY_ROWS),
        }
    }

    fn origin(self) -> u8 {
        match self {
            Segment::Base => 0,
            Segment::Cube => 16,
            Segment::Gray => 232,
        }
    }

    /// The palette index of a (col, row) cell inside this segment.
    fn index_at(self, col: u16, row: u16) -> u8 {
        let (cols, _) = self.shape();
        let offset = match self {
            Segment::Base => row * cols + col,
            // A cube column sweeps red-major/green-minor hue; a row is
            // one blue level — the structure the review asked to see.
            Segment::Cube => row * CUBE_COLS + col,
            Segment::Gray => row * cols + col,
        };
        self.origin() + offset as u8
    }

    /// The segment cell of a palette index; None past the segment.
    fn cell_of(self, index: u8) -> Option<(u16, u16)> {
        let (cols, rows) = self.shape();
        let offset = u16::from(index).checked_sub(u16::from(self.origin()))?;
        let total = cols * rows;
        if offset >= total {
            return None;
        }
        Some((offset % cols, offset / cols))
    }
}

/// The segment holding a palette index (the base, the cube, the grays).
fn segment_of(index: u8) -> Segment {
    match index {
        0..=15 => Segment::Base,
        16..=231 => Segment::Cube,
        _ => Segment::Gray,
    }
}

/// The palette row areas inside the picker's pane — the one layout
/// `draw` and `click_at` share. Later segments clip when the pane runs
/// out of height (navigation keeps working).
pub(crate) fn areas(pane: ratatui::layout::Rect) -> Vec<(Segment, ratatui::layout::Rect)> {
    let mut out = Vec::new();
    let mut y = pane.y + ROWS_ABOVE;
    for segment in SEGMENTS {
        let (cols, rows) = segment.shape();
        if y >= pane.y + pane.height {
            break;
        }
        let height = rows.min(pane.y + pane.height - y);
        out.push((
            segment,
            ratatui::layout::Rect::new(pane.x, y, pane.width.min(cols), height),
        ));
        y += rows;
    }
    out
}

/// Route one key press inside the picker.
pub fn handle_key(state: &mut State, key: KeyEvent) -> Event {
    let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
    match key.code {
        KeyCode::Esc => Event::Cancel,
        KeyCode::Tab if plain => {
            state.focus = match state.focus {
                Focus::Hex => Focus::Grid,
                Focus::Grid => Focus::Hex,
            };
            Event::Consumed
        }
        KeyCode::Left if plain => move_cursor(state, -1, 0),
        KeyCode::Right if plain => move_cursor(state, 1, 0),
        KeyCode::Up if plain => move_cursor(state, 0, -1),
        KeyCode::Down if plain => move_cursor(state, 0, 1),
        KeyCode::Enter if plain => match parse_input(&state.hex) {
            Some(value) => Event::Commit(value),
            None => {
                state.error = Some(format!("bad hex '{}'", state.hex));
                Event::Consumed
            }
        },
        KeyCode::Backspace if plain && state.focus == Focus::Hex => {
            state.hex.pop();
            hex_edited(state)
        }
        KeyCode::Char(ch) if plain && state.focus == Focus::Hex => {
            if state.hex.chars().count() < 7 {
                state.hex.push(ch);
            }
            hex_edited(state)
        }
        _ => Event::Consumed,
    }
}

/// Arrow movement across the segmented palette: within a segment the
/// cursor clamps; at a segment edge it flows into the neighbor segment
/// (base → cube → grays and back), so the whole palette is one keyboard
/// field.
fn move_cursor(state: &mut State, dx: i32, dy: i32) -> Event {
    state.focus = Focus::Grid;
    let segment = segment_of(state.cursor);
    let (cols, rows) = segment.shape();
    let (col, row) = segment.cell_of(state.cursor).unwrap_or((0, 0));
    let (mut new_col, mut new_row) = (
        (i32::from(col) + dx).clamp(0, i32::from(cols) - 1),
        (i32::from(row) + dy).clamp(0, i32::from(rows) - 1),
    );
    let moved = (new_col, new_row) != (i32::from(col), i32::from(row));
    if !moved {
        // An edge press crosses into the neighboring segment: downward
        // enters its first row, upward its last (spatial continuity).
        let crossed = cross_segment(segment, dx, dy);
        if let Some(next) = crossed {
            let (next_cols, next_rows) = next.shape();
            new_col = new_col.min(i32::from(next_cols) - 1);
            new_row = if dy < 0 { i32::from(next_rows) - 1 } else { 0 };
            state.cursor = next.index_at(new_col as u16, new_row as u16);
            state.current = state.cursor_color();
            state.error = None;
            return Event::Preview(state.current);
        }
    }
    state.cursor = segment.index_at(new_col as u16, new_row as u16);
    state.current = state.cursor_color();
    state.error = None;
    Event::Preview(state.current)
}

/// The segment a crossing edge press lands in: down past the base
/// enters the cube, past the cube the grays; up is the reverse. A
/// horizontal press crosses nothing.
fn cross_segment(segment: Segment, dx: i32, dy: i32) -> Option<Segment> {
    let at = SEGMENTS.iter().position(|s| *s == segment)?;
    let next = at as i32 + dy.signum();
    if dx != 0 || next < 0 || next as usize >= SEGMENTS.len() {
        return None;
    }
    SEGMENTS.get(next as usize).copied()
}

/// A hex edit: a complete, valid value previews live; a partial one just
/// waits (never an error while typing).
fn hex_edited(state: &mut State) -> Event {
    match parse_input(&state.hex) {
        Some(value) => {
            state.current = value;
            state.error = None;
            Event::Preview(value)
        }
        None => Event::Consumed,
    }
}

/// The hex field's value, `#`-optional, exactly six hex digits. The
/// indexed colors enter through the grid (clicking or arrows), not the
/// line — a partial hex must wait, never half-commit.
fn parse_input(text: &str) -> Option<ColorValue> {
    let trimmed = text.trim().trim_start_matches('#');
    if trimmed.len() != 6 {
        return None;
    }
    thememodel::parse_hex(trimmed).ok()
}

/// Which picker element a click inside the pane hit; None outside all
/// segments and the hex line.
pub fn click_at(pane: ratatui::layout::Rect, point: (u16, u16)) -> Option<Click> {
    let (col, row) = point;
    if row == pane.y {
        return Some(Click::Hex);
    }
    for (segment, area) in areas(pane) {
        let inside = col >= area.x
            && col < area.x + area.width
            && row >= area.y
            && row < area.y + area.height;
        if !inside {
            continue;
        }
        let (cols, rows) = segment.shape();
        let cell_col = (col - area.x).min(cols - 1);
        let cell_row = (row - area.y).min(rows - 1);
        return Some(Click::Cell(segment.index_at(cell_col, cell_row)));
    }
    None
}

/// The picker's top lines (role + hex, the two preview strips). The
/// footer renders below the palette — `draw` places it there, and an
/// error takes the footer's place.
pub fn lines(state: &State, theme: &Applier) -> Vec<Line<'static>> {
    let accent = theme.style(Role::Text, Role::Surface);
    let dim = theme.style(Role::TextDim, Role::Surface);
    let cursor = if state.focus == Focus::Hex { "_" } else { "" };
    vec![
        Line::from(vec![
            Span::styled(format!(" {} ", role_label(state.role)), accent),
            Span::styled(
                format!("#{}{cursor}", state.hex.trim_start_matches('#')),
                dim,
            ),
        ]),
        preview_line(state, theme),
        second_preview_line(state, theme),
    ]
}

/// The footer line under the palette (the error, when there is one).
pub fn footer_line(state: &State, theme: &Applier) -> Line<'static> {
    match &state.error {
        Some(error) => {
            Line::from(format!(" {error}")).style(theme.style(Role::Danger, Role::Surface))
        }
        None => Line::from(" arrows move · enter commit · tab · esc")
            .style(theme.style(Role::TextDim, Role::Surface)),
    }
}

fn preview_line(state: &State, theme: &Applier) -> Line<'static> {
    let fg = applier_color(state.current);
    Line::from(Span::styled(
        " the role applied on the background ".to_string(),
        ratatui::style::Style::new().fg(fg).bg(theme_bg(theme)),
    ))
}

fn second_preview_line(state: &State, theme: &Applier) -> Line<'static> {
    let bg = applier_color(state.current);
    Line::from(Span::styled(
        " text on the role as a surface ",
        ratatui::style::Style::new().fg(theme_fg(theme)).bg(bg),
    ))
}

fn theme_bg(theme: &Applier) -> ratatui::style::Color {
    theme
        .style(Role::Text, Role::Background)
        .bg
        .unwrap_or_default()
}

fn theme_fg(theme: &Applier) -> ratatui::style::Color {
    theme
        .style(Role::Text, Role::Background)
        .fg
        .unwrap_or_default()
}

fn applier_color(value: ColorValue) -> ratatui::style::Color {
    theme::color_of(value)
}

/// The picker rendered inside `pane` (the section pane it overlays).
/// Short panes clip the lower segments — navigation and the hex line
/// keep working.
pub fn draw(frame: &mut Frame<'_>, pane: ratatui::layout::Rect, state: &State, theme: &Applier) {
    frame.render_widget(Paragraph::new(lines(state, theme)), pane);
    let segment_areas = areas(pane);
    let footer_y = segment_areas
        .last()
        .map(|(_, area)| area.y + area.height)
        .unwrap_or(pane.y + ROWS_ABOVE);
    if footer_y < pane.y + pane.height {
        frame.render_widget(
            Paragraph::new(vec![footer_line(state, theme)]),
            ratatui::layout::Rect::new(pane.x, footer_y, pane.width, 1),
        );
    }
    for (segment, area) in segment_areas {
        let (cols, _) = segment.shape();
        for row in 0..area.height {
            for col in 0..area.width.min(cols) {
                let index = segment.index_at(col, row);
                let (symbol, fg) = if state.focus == Focus::Grid && state.cursor == index {
                    ("◆", applier_color(state.current))
                } else {
                    (" ", applier_color(state.cursor_color()))
                };
                let cell_area = ratatui::layout::Rect::new(area.x + col, area.y + row, 1, 1);
                let cell = Paragraph::new(symbol).style(
                    ratatui::style::Style::new()
                        .fg(fg)
                        .bg(applier_color(ColorValue::Indexed(index))),
                );
                frame.render_widget(cell, cell_area);
            }
        }
    }
}

fn role_label(role: Role) -> &'static str {
    thememodel::role_name(role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::buffer_text;
    use crossterm::event::KeyModifiers;
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn state() -> State {
        State::open(Role::Accent, ColorValue::Rgb(0x7e, 0xe0, 0xa3))
    }

    #[test]
    fn arrows_move_within_and_across_segments() {
        let mut picker = state();
        picker.cursor = 15; // the base row's end
        assert_eq!(
            handle_key(&mut picker, key(KeyCode::Right)),
            Event::Preview(ColorValue::Indexed(15)),
            "the cursor clamps at the base row's right edge"
        );
        // Down leaves the base for the cube's first hue row.
        assert_eq!(
            handle_key(&mut picker, key(KeyCode::Down)),
            Event::Preview(ColorValue::Indexed(16 + 15)),
            "down crosses into the cube, hue column kept"
        );
        assert_eq!(picker.focus, Focus::Grid);
        // The cube's bottom-left corner flows into the gray ramp.
        picker.cursor = 231 - 35; // cube row 5, hue column 0
        assert_eq!(
            handle_key(&mut picker, key(KeyCode::Down)),
            Event::Preview(ColorValue::Indexed(232)),
            "down crosses into the gray ramp"
        );
        // Up from the grays returns to the cube's last hue row.
        assert_eq!(
            handle_key(&mut picker, key(KeyCode::Up)),
            Event::Preview(ColorValue::Indexed(16 + 5 * 36)),
            "up crosses back into the cube's bottom row"
        );
    }

    #[test]
    fn hex_edits_preview_when_complete_and_wait_when_partial() {
        let mut picker = state();
        picker.focus = Focus::Hex;
        picker.hex.clear();
        for (at, ch) in "106b4a".chars().enumerate() {
            let event = handle_key(&mut picker, key(KeyCode::Char(ch)));
            if at == 5 {
                assert_eq!(
                    event,
                    Event::Preview(ColorValue::Rgb(0x10, 0x6b, 0x4a)),
                    "a complete hex previews live"
                );
            } else {
                assert_eq!(event, Event::Consumed, "partial input waits");
            }
        }
    }

    #[test]
    fn enter_commits_and_a_bad_hex_refuses_in_place() {
        let mut picker = state();
        picker.hex = "#106b4a".into();
        assert_eq!(
            handle_key(&mut picker, key(KeyCode::Enter)),
            Event::Commit(ColorValue::Rgb(0x10, 0x6b, 0x4a))
        );
        picker.hex = "zz".into();
        assert_eq!(
            handle_key(&mut picker, key(KeyCode::Enter)),
            Event::Consumed,
            "a bad hex stays in the picker"
        );
        assert!(picker.error.is_some());
        assert_eq!(handle_key(&mut picker, key(KeyCode::Esc)), Event::Cancel);
    }

    #[test]
    fn tab_flips_focus_and_esc_cancels() {
        let mut picker = state();
        handle_key(&mut picker, key(KeyCode::Tab));
        assert_eq!(picker.focus, Focus::Hex);
        assert_eq!(handle_key(&mut picker, key(KeyCode::Esc)), Event::Cancel);
    }

    #[test]
    fn segment_layout_covers_all_256_indices() {
        // Every index maps into exactly one segment, and back.
        for index in 0..=u8::MAX {
            let segment = segment_of(index);
            let (col, row) = segment.cell_of(index).expect("inside its segment");
            assert_eq!(
                segment.index_at(col, row),
                index,
                "index {index} round-trips"
            );
        }
        assert!(Segment::Base.cell_of(16).is_none());
        assert!(Segment::Cube.cell_of(232).is_none());
    }

    #[test]
    fn click_lands_on_the_drawn_cell_across_segments() {
        let pane = ratatui::layout::Rect::new(2, 3, 40, 24);
        let base = &areas(pane)[0];
        assert_eq!(base.1.y, pane.y + 3, "the base row sits under the strips");
        assert_eq!(
            click_at(pane, (base.1.x + 5, base.1.y)),
            Some(Click::Cell(5)),
            "a base-row click lands on the drawn cell"
        );
        let cube = &areas(pane)[1];
        // Cube cell hue column 8 (r=1,g=2), blue row 2 → 16 + 2*36 + 8.
        assert_eq!(
            click_at(pane, (cube.1.x + 8, cube.1.y + 2)),
            Some(Click::Cell(16 + 2 * 36 + 8))
        );
        assert_eq!(click_at(pane, (2, 3)), Some(Click::Hex));
        assert_eq!(click_at(pane, (2, 2)), None, "above the pane");
    }

    #[test]
    fn drawn_picker_shows_role_hex_palette_and_footer() {
        let theme = Applier::fallback();
        let mut picker = state();
        picker.cursor = 91;
        let backend = TestBackend::new(40, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| draw(f, ratatui::layout::Rect::new(0, 0, 40, 24), &picker, &theme))
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("accent"), "role label missing: {text:?}");
        assert!(text.contains("#7ee0a3"), "hex value missing: {text:?}");
        assert!(text.contains("enter commit"), "footer missing: {text:?}");
    }
}
