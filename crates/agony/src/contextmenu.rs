//! The context menu (Part09 §9.3): one small bordered widget at the
//! pointer, hung on Part06's `context_target` seam. Three targets feed
//! it — a PROGRAMS row (Open, Pin to desktop, Copy launch command), a
//! desktop tile (Open, Unpin/Delete document, Copy path) — and every
//! other target stays inert (the seam routes, nothing renders).
//! Keyboard-navigable (the pointer's parity rule); Esc or an outside
//! click closes.
//!
//! Items: `Menu` — the open menu (items, selection, anchor cell);
//! `Item` — one row (label + the action tag); `Action` — what the app
//! executes when the row activates; `Event` — `Chosen(Action)`, `Close`,
//! `Consumed`.
//! Core functions: `Menu::open_at` — build with items at an anchor;
//! `handle_key` — Up/Down/Enter/Esc only (a modal's modal);
//! `rect` — the clamped overlay rect at the anchor; `item_at` — the
//! click lookup; `action_at` — the action a click activates; `draw` —
//! the widget.
//! Depends on: agony_core::layout (Rect), theme.
//! Called by: app (open on right-click, keys, clicks, draw).
//! Calls: nothing async — the actions are the app's to execute.

use agony_core::layout::Rect;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::theme::{Applier, Role};

/// The actions the app executes behind a context row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Launch a program row, or open a tile (shortcut or document).
    Open,
    /// Write the shortcut stub onto the AGony desktop.
    Pin,
    /// Put the launch command on the status line for picking up.
    CopyCommand,
    /// Remove a shortcut stub from the AGony desktop.
    Unpin,
    /// Refused until a trash exists — the app says so.
    DeleteDocument,
    /// Put a tile's path on the status line.
    CopyPath,
}

/// One row: the label and the action it activates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub action: Action,
}

impl Item {
    pub fn new(label: impl Into<String>, action: Action) -> Item {
        Item {
            label: label.into(),
            action,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Consumed,
    Close,
    Chosen(Action),
}

/// The open context menu: its rows, the selection, the anchor cell.
#[derive(Clone, Debug)]
pub struct Menu {
    pub items: Vec<Item>,
    pub selected: usize,
    pub anchor: (u16, u16),
}

impl Menu {
    pub fn open_at(items: Vec<Item>, anchor: (u16, u16)) -> Menu {
        Menu {
            items,
            selected: 0,
            anchor,
        }
    }

    /// Route one key press: the modal's keys only.
    pub fn handle_key(&mut self, key: KeyEvent) -> Event {
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Esc => Event::Close,
            KeyCode::Up if plain => {
                self.selected = self.selected.saturating_sub(1);
                Event::Consumed
            }
            KeyCode::Down if plain => {
                let last = self.items.len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
                Event::Consumed
            }
            KeyCode::Enter if plain => match self.items.get(self.selected) {
                Some(item) => Event::Chosen(item.action),
                None => Event::Close,
            },
            _ => Event::Consumed,
        }
    }

    /// The overlay's rect at the anchor, clamped into `bounds` (a menu
    /// near the screen's edge grows inward, never off-screen).
    pub fn rect(&self, bounds: Rect) -> Rect {
        let width = self
            .items
            .iter()
            .map(|item| item.label.chars().count() as u16 + 3)
            .max()
            .unwrap_or(1)
            .max(14);
        let height = self.items.len() as u16 + 2;
        let width = width.min(bounds.width.max(1));
        let height = height.min(bounds.height.max(1));
        let x = (self.anchor.0).min(bounds.right().saturating_sub(width).max(bounds.x));
        let y = (self.anchor.1).min(bounds.bottom().saturating_sub(height).max(bounds.y));
        Rect::new(x, y, width, height)
    }

    /// The selected row's index under a click, or None outside.
    pub fn item_at(&self, bounds: Rect, point: (u16, u16)) -> Option<usize> {
        let rect = self.rect(bounds);
        let (col, row) = point;
        let inside = col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height;
        if !inside {
            return None;
        }
        let at = row.checked_sub(rect.y)?.checked_sub(1)? as usize;
        self.items.get(at).map(|_| at)
    }

    /// The action under a click, or None outside — the mouse half of
    /// Enter.
    pub fn action_at(&self, bounds: Rect, point: (u16, u16)) -> Option<Action> {
        let index = self.item_at(bounds, point)?;
        self.items.get(index).map(|item| item.action)
    }
}

pub fn draw(frame: &mut Frame<'_>, bounds: Rect, menu: &Menu, theme: &Applier) {
    let rect = menu.rect(bounds);
    // The menu owns every cell of its rect: Clear first, so the windows
    // under it never bleed through (the UI review's #1 finding).
    frame.render_widget(ratatui::widgets::Clear, rect);
    let block = Block::bordered()
        .border_style(theme.style(Role::Border, Role::Surface))
        .style(theme.style(Role::Text, Role::Surface));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let lines: Vec<Line<'static>> = menu
        .items
        .iter()
        .enumerate()
        .map(|(at, item)| {
            let style = if at == menu.selected {
                theme.style(Role::Background, Role::Selection)
            } else {
                theme.style(Role::Text, Role::Surface)
            };
            Line::from(format!(" {} ", item.label)).style(style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
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

    fn menu() -> Menu {
        Menu::open_at(
            vec![
                Item::new("Open", Action::Open),
                Item::new("Pin to desktop", Action::Pin),
                Item::new("Copy launch command", Action::CopyCommand),
            ],
            (20, 10),
        )
    }

    #[test]
    fn keys_walk_and_choose() {
        let mut state = menu();
        assert_eq!(state.handle_key(key(KeyCode::Down)), Event::Consumed);
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            Event::Chosen(Action::Pin)
        );
        let mut again = menu();
        again.handle_key(key(KeyCode::Down));
        again.handle_key(key(KeyCode::Down));
        again.handle_key(key(KeyCode::Down));
        assert_eq!(again.selected, 2, "clamped at the last row");
        assert_eq!(again.handle_key(key(KeyCode::Esc)), Event::Close);
    }

    #[test]
    fn an_empty_menu_closes_on_enter() {
        let mut state = Menu::open_at(Vec::new(), (0, 0));
        assert_eq!(state.handle_key(key(KeyCode::Enter)), Event::Close);
    }

    #[test]
    fn the_rect_clamps_into_the_bounds() {
        let state = menu();
        let bounds = Rect::new(0, 0, 80, 24);
        let rect = state.rect(bounds);
        assert!(rect.x + rect.width <= bounds.right(), "fits the width");
        assert!(rect.y + rect.height <= bounds.bottom(), "fits the height");
        let corner = Menu::open_at(state.items.clone(), (75, 22));
        let clamped = corner.rect(bounds);
        assert!(clamped.right() <= bounds.right());
        assert!(clamped.bottom() <= bounds.bottom(), "grows inward");
    }

    #[test]
    fn clicks_find_rows_and_miss_outside() {
        let bounds = Rect::new(0, 0, 80, 24);
        let state = menu();
        let rect = state.rect(bounds);
        assert_eq!(
            state.item_at(bounds, (rect.x + 2, rect.y + 2)),
            Some(1),
            "the second row (inside the border)"
        );
        assert_eq!(state.item_at(bounds, (rect.x + 2, rect.y + 1)), Some(0));
        assert_eq!(
            state.item_at(bounds, (rect.x + 2, rect.y)),
            None,
            "the border is not a row"
        );
        assert_eq!(state.item_at(bounds, (0, 0)), None, "away from the menu");
    }

    #[test]
    fn a_click_on_a_row_carries_its_action() {
        let bounds = Rect::new(0, 0, 80, 24);
        let state = menu();
        let rect = state.rect(bounds);
        assert_eq!(
            state.action_at(bounds, (rect.x + 2, rect.y + 1)),
            Some(Action::Open),
            "the first row's action"
        );
        assert_eq!(
            state.action_at(bounds, (rect.x + 2, rect.y + 2)),
            Some(Action::Pin)
        );
        assert_eq!(
            state.action_at(bounds, (rect.x + 2, rect.y)),
            None,
            "the border activates nothing"
        );
        assert_eq!(state.action_at(bounds, (0, 0)), None, "outside closes");
    }

    #[test]
    fn drawn_menu_shows_the_rows() {
        let theme = Applier::fallback();
        let state = menu();
        let backend = TestBackend::new(40, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| draw(f, Rect::new(0, 0, 40, 24), &state, &theme))
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("Pin to desktop"), "{text:?}");
        assert!(text.contains("Copy launch command"), "{text:?}");
    }
}
