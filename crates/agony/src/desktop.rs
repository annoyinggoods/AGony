//! The desktop surface: the theme's background under the windows. The
//! pane-hosting days ended with Part04 — windows (wm) carry the panes;
//! the desktop is the canvas they float on.
//!
//! Core functions: `draw` — fill the desktop region with the background
//! role.
//! Depends on: theme (colors).
//! Called by: app's draw pass (first, under everything).
//! Calls: nothing.

use agony_core::layout::Rect;
use ratatui::{widgets::Paragraph, Frame};

use crate::theme::{Applier, Role};

pub fn draw(frame: &mut Frame<'_>, region: Rect, theme: &Applier) {
    let background = Paragraph::new("").style(theme.style(Role::Text, Role::Background));
    frame.render_widget(background, region);
}
