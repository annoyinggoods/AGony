//! Pure window-manager math: the frame regions and the full window model
//! (Part04). The core owns rects, z-order and window state; the shell's
//! reconciler maps windows onto daemon panes.
//!
//! Items: `Window` — one window (id, title, rect, z, state, restore rect,
//! pane id); `WinState` — normal/minimized/maximized; `WmState` — every
//! window plus focus and the desktop region; `Hit` / `TitleButton` /
//! `Edge` — what a click landed on; `FrameRegions` — the
//! desktop/taskbar/overlay split of the screen.
//! Core functions: `frame_regions` — split a screen into regions (the
//! only place screen rectangles are divided); `WmState::open`/`open_at`/
//! `close`/`focus`/`focus_next`/`focus_prev`/`minimize`/`restore`/
//! `toggle_maximize`/`move_by`/`resize_edge`/`bring_to_front` — the pure
//! operations (none await, spawn, or draw); `visible_windows` /
//! `focus_order` — window ids, top of the z-order first; `effective_rect`
//! / `inner_rect` — the geometry the renderer and the reconciler use;
//! `hit_test` — the one click-classification table.
//! Depends on: nothing (ratatui types only).
//! Called by: the shell's `frame`, `wm`, `reconcile` and `app` modules.
//! Calls: nothing.

use ratatui::layout::Margin;
pub use ratatui::layout::Rect;

/// The smallest window (outer cells): the inner pane keeps 6x4 usable
/// cells (Part04's pitfall table).
pub const MIN_WINDOW_WIDTH: u16 = 8;
pub const MIN_WINDOW_HEIGHT: u16 = 6;
/// The titlebar's button zone: the rightmost columns of the top row
/// (three two-cell buttons plus one leading separator — shared by the
/// renderer and the hit-test).
pub const TITLE_BUTTON_COLS: u16 = 7;
/// How many cascade steps before the offset wraps back to the origin.
const CASCADE_STEPS: u16 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinState {
    Normal,
    Minimized,
    Maximized,
}

/// One window as the window manager sees it: pure data, no behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub id: u64,
    pub title: String,
    /// Geometry while not maximized (kept while minimized, so restore
    /// returns the exact rect).
    pub rect: Rect,
    pub z: u16,
    pub state: WinState,
    /// The pre-maximize rect, remembered so un-maximize returns to it.
    pub restore: Rect,
    /// The hosted pane's id (engine-agnostic; the shell reconciles it), or
    /// None for an internal window (the file panel renders a widget, not a
    /// pane).
    pub pane_id: Option<u32>,
}

/// The screen split the frame hands out: desktop, bottom taskbar, and the
/// overlay layer that floats above the desktop (same area, higher layer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRegions {
    pub desktop: Rect,
    pub taskbar: Rect,
    pub overlay: Rect,
}

/// One of the titlebar's window buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleButton {
    Minimize,
    Maximize,
    Close,
}

/// A resizeable border edge. `Top` exists for the operation's
/// completeness; `hit_test` never returns it (the top border row is the
/// titlebar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Left,
    Bottom,
    Right,
}

/// What a click on the desktop landed on — the shell's mouse handler is a
/// `match` over this, never arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    TitleBar { window: u64 },
    Button { window: u64, button: TitleButton },
    Edge { window: u64, edge: Edge },
    Interior { window: u64 },
}

/// All windows plus the focus and the desktop region they live in.
/// Every operation except [`WmState::bring_to_front`] keeps the
/// invariants the `invariant_` tests pin: the focused window is the top
/// of the z-order and visible, minimized windows never render,
/// non-maximized rects stay inside the desktop (clamped), ids never
/// repeat. `bring_to_front` raises without focusing — its callers (the
/// reconciler's adoption) pair it with `focus` immediately.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WmState {
    pub windows: Vec<Window>,
    pub focus: Option<u64>,
    next_id: u64,
    opens: u64,
    pub desktop: Rect,
}

impl WmState {
    pub fn new(desktop: Rect) -> WmState {
        WmState {
            windows: Vec::new(),
            focus: None,
            next_id: 1,
            opens: 0,
            desktop,
        }
    }

    /// The desktop changed (terminal resize or first layout): re-clamp
    /// every window into it. Maximized windows need nothing — their rect
    /// is derived from the desktop at render time.
    pub fn set_desktop(&mut self, desktop: Rect) {
        self.desktop = desktop;
        for window in &mut self.windows {
            window.rect = clamp_into(window.rect, desktop);
            window.restore = clamp_into(window.restore, desktop);
        }
    }

    /// Open a window at the cascade position: each window offset by two
    /// cells from the last, wrapping after `CASCADE_STEPS`, clamped,
    /// focused, on top. Returns the new id.
    pub fn open(&mut self, title: &str, pane_id: Option<u32>) -> u64 {
        let step = (self.opens % u64::from(CASCADE_STEPS)) as u16 * 2;
        self.open_at(title, pane_id, default_rect(self.desktop, step))
    }

    /// Open a window at an explicit rect (the panel's docked default, an
    /// adopted pane) — same clamping, focusing and raising as `open`.
    pub fn open_at(&mut self, title: &str, pane_id: Option<u32>, rect: Rect) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.opens = self.opens.saturating_add(1);
        let rect = clamp_into(rect, self.desktop);
        self.windows.push(Window {
            id,
            title: title.to_string(),
            rect,
            z: 0,
            state: WinState::Normal,
            restore: rect,
            pane_id,
        });
        self.focus(id);
        id
    }

    /// Close a window: forget it; focus moves to the next visible window,
    /// top of the z-order first. Killing the pane is the reconciler's job.
    pub fn close(&mut self, id: u64) {
        self.windows.retain(|window| window.id != id);
        if self.focus == Some(id) {
            self.focus = self.visible_windows().first().copied();
        }
    }

    /// Activate a window: minimize-restore, raise, focus — the one call
    /// behind taskbar clicks and `Ctrl-E`.
    pub fn focus(&mut self, id: u64) {
        let Some(window) = self.windows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        if window.state == WinState::Minimized {
            window.state = WinState::Normal;
        }
        bring_to_front(&mut self.windows, id);
        self.focus = Some(id);
    }

    /// Cycle focus to the next window in open order (the taskbar's
    /// left-to-right order; wraps). Visible windows cycle first; the
    /// minimized ones join at the tail of the cycle and are restored on
    /// arrival, like Alt-Tab — so a just-minimized window is never the
    /// immediate next stop. Raising on focus would make z-order cycling
    /// degenerate (it alternates between two windows), so the cycle
    /// order is the stable open order.
    pub fn focus_next(&mut self) {
        self.cycle_focus(1);
    }

    /// Cycle focus to the previous window in open order (wraps; the
    /// minimized tail is reached last and restored on arrival).
    pub fn focus_prev(&mut self) {
        self.cycle_focus(-1);
    }

    fn cycle_focus(&mut self, direction: i32) {
        let ids: Vec<(u64, bool)> = self
            .windows
            .iter()
            .map(|w| (w.id, w.state == WinState::Minimized))
            .collect();
        let visible = || ids.iter().filter(|(_, minimized)| !*minimized);
        let minimized = || ids.iter().filter(|(_, minimized)| *minimized);
        let order: Vec<u64> = visible().chain(minimized()).map(|(id, _)| *id).collect();
        let Some(current) = self.focus.filter(|id| order.contains(id)) else {
            return;
        };
        let at = order.iter().position(|id| *id == current).unwrap_or(0);
        let len = order.len() as i32;
        let moved = (at as i32 + direction).rem_euclid(len) as usize;
        let next = order[moved];
        self.focus(next);
    }

    /// Park a window in the taskbar: the rect is kept, nothing cleared;
    /// if it was focused, focus moves to the next visible window.
    pub fn minimize(&mut self, id: u64) {
        let Some(window) = self.windows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        if window.state == WinState::Maximized {
            window.rect = window.restore;
        }
        window.state = WinState::Minimized;
        if self.focus == Some(id) {
            self.focus = self.visible_windows().first().copied();
        }
    }

    /// Bring a minimized window back: the same call as focusing (the
    /// window manager keeps every stored rect clamped — `set_desktop`
    /// reclamps on every resize — so restore carries no geometry of its
    /// own; kept as a named entry point for the taskbar's restore click).
    pub fn restore(&mut self, id: u64) {
        self.focus(id);
    }

    /// Maximize fills the desktop region and remembers the restore rect;
    /// again returns to it. The rect is derived from the desktop at
    /// render time, never stored (a shrink cannot strand the window).
    pub fn toggle_maximize(&mut self, id: u64) {
        let Some(window) = self.windows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        match window.state {
            WinState::Maximized => {
                window.state = WinState::Normal;
                window.rect = clamp_into(window.restore, self.desktop);
            }
            WinState::Minimized => return,
            WinState::Normal => {
                window.state = WinState::Maximized;
                window.restore = window.rect;
            }
        }
        self.focus(id);
    }

    /// Drag-move a normal window by a cell delta, clamped at the desktop
    /// edges. No-op on maximized or minimized windows.
    pub fn move_by(&mut self, id: u64, dx: i32, dy: i32) {
        let desktop = self.desktop;
        let Some(window) = self.normal_window_mut(id) else {
            return;
        };
        let moved = offset_rect(window.rect, dx, dy);
        window.rect = clamp_into(moved, desktop);
    }

    /// Drag one edge by a cell delta (positive grows), clamped to the
    /// desktop and the minimum size. No-op on maximized or minimized
    /// windows.
    pub fn resize_edge(&mut self, id: u64, edge: Edge, delta: i32) {
        let desktop = self.desktop;
        let Some(window) = self.normal_window_mut(id) else {
            return;
        };
        let resized = resize_rect(window.rect, edge, delta, desktop);
        window.rect = clamp_into(resized, desktop);
    }

    /// Raise a window above every other *without* focusing it (kept
    /// public: the reconciler and tests reach for it directly). The one
    /// operation that breaks the focus-top invariant — callers pair it
    /// with `focus` (see the struct's doc).
    pub fn bring_to_front(&mut self, id: u64) {
        bring_to_front(&mut self.windows, id);
    }

    pub fn window(&self, id: u64) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    /// Record (or clear) the pane a window hosts — the reconciler's
    /// callback after a spawn or an adoption.
    pub fn set_pane(&mut self, id: u64, pane_id: Option<u32>) {
        if let Some(window) = self.windows.iter_mut().find(|w| w.id == id) {
            window.pane_id = pane_id;
        }
    }

    /// Rename a window (the panel's held-file marker is the first user).
    pub fn set_title(&mut self, id: u64, title: &str) {
        if let Some(window) = self.windows.iter_mut().find(|w| w.id == id) {
            window.title = title.to_string();
        }
    }

    pub fn focused(&self) -> Option<&Window> {
        self.focus.and_then(|id| self.window(id))
    }

    fn normal_window_mut(&mut self, id: u64) -> Option<&mut Window> {
        self.windows
            .iter_mut()
            .find(|w| w.id == id && w.state == WinState::Normal)
    }

    /// Non-minimized window ids, top of the z-order first. The renderer
    /// paints the reverse of this so higher windows cover lower ones.
    pub fn visible_windows(&self) -> Vec<u64> {
        ordered_ids(&self.windows, false)
    }

    /// All window ids ordered for focus cycling: top of the z-order
    /// first, minimized windows included.
    pub fn focus_order(&self) -> Vec<u64> {
        ordered_ids(&self.windows, true)
    }
}

/// Split `area` into desktop + taskbar + overlay. The taskbar is always
/// exactly `taskbar_rows` tall; any odd remainder goes to the desktop.
/// `position` decides which edge carries it (Part08's taskbar-position
/// setting): bottom keeps the desktop above it, top puts the taskbar on
/// the first rows and the desktop under it.
pub fn frame_regions(
    area: Rect,
    taskbar_rows: u16,
    position: crate::config::TaskbarPosition,
) -> FrameRegions {
    use crate::config::TaskbarPosition as Position;
    let rows = taskbar_rows.min(area.height);
    let desktop_height = area.height - rows;
    match position {
        Position::Top => FrameRegions {
            taskbar: Rect::new(area.x, area.y, area.width, rows),
            desktop: Rect::new(
                area.x,
                area.y.saturating_add(rows),
                area.width,
                desktop_height,
            ),
            overlay: Rect::new(
                area.x,
                area.y.saturating_add(rows),
                area.width,
                desktop_height,
            ),
        },
        Position::Bottom => {
            let desktop = Rect::new(area.x, area.y, area.width, desktop_height);
            FrameRegions {
                taskbar: Rect::new(
                    area.x,
                    area.y.saturating_add(desktop_height),
                    area.width,
                    rows,
                ),
                overlay: desktop,
                desktop,
            }
        }
    }
}

/// The rect a window occupies on screen: the desktop region when
/// maximized (derived, never stored), the stored rect otherwise.
pub fn effective_rect(window: &Window, desktop: Rect) -> Rect {
    if window.state == WinState::Maximized {
        desktop
    } else {
        window.rect
    }
}

/// The pane/client area inside a window's chrome (a one-cell border ring).
pub fn inner_rect(rect: Rect) -> Rect {
    rect.inner(Margin::new(1, 1))
}

/// Classify a desktop cell (col, row): the topmost window under it, and
/// which zone of it. The top border row is the titlebar (buttons in its
/// rightmost `TITLE_BUTTON_COLS` cells); the other three borders are
/// resize edges; everything inside is the interior.
pub fn hit_test(wm: &WmState, col: u16, row: u16) -> Hit {
    let desktop = wm.desktop;
    let top = wm.visible_windows().into_iter().find_map(|id| {
        let rect = effective_rect(wm.window(id)?, desktop);
        contains(rect, col, row).then_some((id, rect))
    });
    let Some((window, rect)) = top else {
        return Hit::None;
    };
    classify(rect, col, row, window)
}

fn classify(rect: Rect, col: u16, row: u16, window: u64) -> Hit {
    let on_top_row = row == rect.y;
    let on_bottom_row = row == rect.y + rect.height.saturating_sub(1);
    let on_left = col == rect.x;
    let on_right = col == rect.x + rect.width.saturating_sub(1);
    if on_top_row {
        return titlebar_hit(rect, col, window);
    }
    if on_bottom_row || on_left || on_right {
        return Hit::Edge {
            window,
            edge: edge_at(on_left, on_right, on_bottom_row),
        };
    }
    Hit::Interior { window }
}

fn titlebar_hit(rect: Rect, col: u16, window: u64) -> Hit {
    let buttons_start = rect.right().saturating_sub(1 + TITLE_BUTTON_COLS);
    if col < buttons_start {
        return Hit::TitleBar { window };
    }
    Hit::Button {
        window,
        button: button_at(col - buttons_start),
    }
}

fn button_at(offset: u16) -> TitleButton {
    match (offset / 2).min(2) {
        0 => TitleButton::Minimize,
        1 => TitleButton::Maximize,
        _ => TitleButton::Close,
    }
}

fn edge_at(left: bool, right: bool, bottom: bool) -> Edge {
    if bottom {
        Edge::Bottom
    } else if left {
        Edge::Left
    } else if right {
        Edge::Right
    } else {
        Edge::Top
    }
}

fn contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

// ------------------------------------------------------------- geometry

/// Translate a rect by cell deltas.
fn offset_rect(rect: Rect, dx: i32, dy: i32) -> Rect {
    Rect::new(
        clamp_u16(rect.x as i32 + dx),
        clamp_u16(rect.y as i32 + dy),
        rect.width,
        rect.height,
    )
}

/// Move one edge by `delta` (positive grows), keeping the minimum size
/// and the desktop's bounds.
fn resize_rect(rect: Rect, edge: Edge, delta: i32, desktop: Rect) -> Rect {
    match edge {
        Edge::Left => {
            let max_x = rect.right().saturating_sub(MIN_WINDOW_WIDTH);
            let x = clamp_i32(rect.x as i32 + delta, desktop.x as i32, max_x as i32);
            Rect::new(
                clamp_u16(x),
                rect.y,
                rect.right() - clamp_u16(x),
                rect.height,
            )
        }
        Edge::Right => {
            let max_w = desktop.right().saturating_sub(rect.x).max(MIN_WINDOW_WIDTH);
            let w = clamp_i32(
                rect.width as i32 + delta,
                MIN_WINDOW_WIDTH as i32,
                max_w as i32,
            );
            Rect::new(rect.x, rect.y, clamp_u16(w), rect.height)
        }
        Edge::Top => {
            let max_y = rect.bottom().saturating_sub(MIN_WINDOW_HEIGHT);
            let y = clamp_i32(rect.y as i32 + delta, desktop.y as i32, max_y as i32);
            Rect::new(
                rect.x,
                clamp_u16(y),
                rect.width,
                rect.bottom() - clamp_u16(y),
            )
        }
        Edge::Bottom => {
            let max_h = desktop
                .bottom()
                .saturating_sub(rect.y)
                .max(MIN_WINDOW_HEIGHT);
            let h = clamp_i32(
                rect.height as i32 + delta,
                MIN_WINDOW_HEIGHT as i32,
                max_h as i32,
            );
            Rect::new(rect.x, rect.y, rect.width, clamp_u16(h))
        }
    }
}

/// Clamp a rect so it lies inside `bounds` (position clamped; size never
/// below the minimum and never past the bounds' far edge). A rect larger
/// than the bounds pins to the bounds' origin — containment is
/// best-effort below the minimum size, never a wrap.
fn clamp_into(rect: Rect, bounds: Rect) -> Rect {
    let max_w = bounds.width.max(MIN_WINDOW_WIDTH);
    let max_h = bounds.height.max(MIN_WINDOW_HEIGHT);
    let width = rect.width.clamp(MIN_WINDOW_WIDTH, max_w);
    let height = rect.height.clamp(MIN_WINDOW_HEIGHT, max_h);
    let x = clamp_u16(clamp_i32(
        rect.x as i32,
        bounds.x as i32,
        (bounds.x + bounds.width.saturating_sub(width)) as i32,
    ));
    let y = clamp_u16(clamp_i32(
        rect.y as i32,
        bounds.y as i32,
        (bounds.y + bounds.height.saturating_sub(height)) as i32,
    ));
    Rect::new(x, y, width, height)
}

/// The default open rect: three quarters of the desktop, offset by the
/// cascade step, clamped (a degenerate desktop pins the minimum size at
/// the origin — containment is best-effort below the minimum).
fn default_rect(desktop: Rect, step: u16) -> Rect {
    let width = (desktop.width * 3 / 4)
        .max(MIN_WINDOW_WIDTH)
        .min(desktop.width.max(MIN_WINDOW_WIDTH));
    let height = (desktop.height * 3 / 4)
        .max(MIN_WINDOW_HEIGHT)
        .min(desktop.height.max(MIN_WINDOW_HEIGHT));
    let x = desktop.x + step.min(desktop.width.saturating_sub(width));
    let y = desktop.y + step.min(desktop.height.saturating_sub(height));
    Rect::new(x, y, width, height)
}

fn clamp_i32(value: i32, low: i32, high: i32) -> i32 {
    value.clamp(low.min(high), high.max(low))
}

fn clamp_u16(value: i32) -> u16 {
    value.clamp(0, u16::MAX as i32) as u16
}

// -------------------------------------------------------------- z-order

/// Raise the window with `id` above every other window.
fn bring_to_front(windows: &mut [Window], id: u64) {
    if windows.is_empty() {
        return;
    }
    if max_z(windows) == u16::MAX {
        normalize_z(windows);
    }
    let next = max_z(windows) + 1;
    if let Some(window) = windows.iter_mut().find(|w| w.id == id) {
        window.z = next;
    }
}

fn ordered_ids(windows: &[Window], include_minimized: bool) -> Vec<u64> {
    let mut picked: Vec<&Window> = windows
        .iter()
        .filter(|w| include_minimized || w.state != WinState::Minimized)
        .collect();
    picked.sort_by(|a, b| b.z.cmp(&a.z).then(a.id.cmp(&b.id)));
    picked.into_iter().map(|w| w.id).collect()
}

fn max_z(windows: &[Window]) -> u16 {
    windows.iter().map(|w| w.z).max().unwrap_or(0)
}

/// Re-space z values to 1..=n in the current order, so `bring_to_front`
/// cannot overflow.
fn normalize_z(windows: &mut [Window]) {
    let mut order: Vec<u64> = windows.iter().map(|w| w.id).collect();
    order.sort_by_key(|&id| z_of(windows, id));
    for (z, id) in order.into_iter().enumerate() {
        if let Some(window) = windows.iter_mut().find(|w| w.id == id) {
            window.z = z as u16 + 1;
        }
    }
}

fn z_of(windows: &[Window], id: u64) -> u16 {
    windows.iter().find(|w| w.id == id).map_or(0, |w| w.z)
}

// -------------------------------------------------------- icon grid (Part09)

/// A column-major tile grid over the desktop (Part09 9.4): tiles fill a
/// column top to bottom, then the next column right — reading order for a
/// vertical list of icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconGrid {
    /// The region the grid tiles (the desktop).
    pub origin: Rect,
    pub cols: u16,
    pub rows: u16,
    pub tile_w: u16,
    pub tile_h: u16,
}

/// The column-major grid of `tile_w × tile_h` tiles over `desktop`; a
/// degenerate region or tile yields at least one column and row.
pub fn icon_grid(desktop: Rect, tile_w: u16, tile_h: u16) -> IconGrid {
    let cols = (desktop.width / tile_w.max(1)).max(1);
    let rows = (desktop.height / tile_h.max(1)).max(1);
    IconGrid {
        origin: desktop,
        cols,
        rows,
        tile_w,
        tile_h,
    }
}

impl IconGrid {
    /// The grid's tile capacity (the caller bounds it by the entry count).
    pub fn count(&self) -> usize {
        self.cols as usize * self.rows as usize
    }

    /// The rect of tile `index` in column-major order, or None past the
    /// grid's capacity.
    pub fn rect(&self, index: usize) -> Option<Rect> {
        if index >= self.count() {
            return None;
        }
        let rows = self.rows.max(1) as usize;
        let (col, row) = (index / rows, index % rows);
        Some(Rect::new(
            self.origin.x + (col as u16) * self.tile_w,
            self.origin.y + (row as u16) * self.tile_h,
            self.tile_w,
            self.tile_h,
        ))
    }

    /// The tile index under a point, or None when the point falls outside
    /// the grid's cells (an empty tail of the last column is outside).
    pub fn tile_at(&self, col: u16, row: u16) -> Option<usize> {
        let (x, y) = (
            col.checked_sub(self.origin.x)?,
            row.checked_sub(self.origin.y)?,
        );
        let (tile_col, tile_row) = (x / self.tile_w.max(1), y / self.tile_h.max(1));
        let inside = tile_col < self.cols && tile_row < self.rows;
        inside.then_some(tile_row as usize + tile_col as usize * self.rows.max(1) as usize)
    }
}

/// The icon grid's keyboard state: one selected index moved by reading
/// order and column steps that wrap at the edges (Part09 9.4.4).
/// Column-major reading order: Down/Up step one tile (a column's bottom
/// continues into the next column's top); Left/Right step one column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridCursor {
    pub selected: usize,
}

impl GridCursor {
    pub fn new() -> GridCursor {
        GridCursor { selected: 0 }
    }

    /// Step `rows` tiles in reading order and `columns` columns (each
    /// may be negative), wrapping at the list's ends and the grid's
    /// edges, bounded by the entry `count`. A no-op when nothing is
    /// listed.
    pub fn move_by(&mut self, grid: &IconGrid, count: usize, columns: i32, rows: i32) {
        if count == 0 {
            return;
        }
        if rows != 0 {
            let step = rows.rem_euclid(count as i32) as usize;
            self.selected = (self.selected + step) % count;
        }
        if columns != 0 {
            let height = grid.rows.max(1) as i32;
            let width = grid.cols.max(1) as i32;
            let col = self.selected as i32 / height;
            let row = self.selected as i32 % height;
            let moved = (col + columns).rem_euclid(width);
            let candidate = (moved * height + row) as usize;
            self.selected = candidate.min(count - 1);
        }
    }

    /// After the entry list shrinks, keep the selection inside it.
    pub fn clamp(&mut self, count: usize) {
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    fn wm_with(count: usize) -> WmState {
        let mut wm = WmState::new(desktop());
        for n in 0..count {
            wm.open(&format!("w{n}"), Some(n as u32));
        }
        wm
    }
    // (mut kept: opens mutate)

    // ------------------------------------------------------- invariants

    /// Part04 4.2.2: after any operation sequence, the focused window is
    /// visible and at the top of the z-order.
    #[test]
    fn invariant_focus_is_top_of_z_order() {
        let mut wm = wm_with(3);
        let ops: [fn(&mut WmState); 6] = [
            |wm| wm.focus_next(),
            |wm| wm.focus_prev(),
            |wm| {
                let id = wm.visible_windows()[1];
                wm.minimize(id);
            },
            |wm| {
                let id = wm.visible_windows()[0];
                wm.close(id);
            },
            |wm| {
                let id = wm.visible_windows()[0];
                wm.toggle_maximize(id);
            },
            |wm| wm.focus(2),
        ];
        for op in ops {
            op(&mut wm);
            let visible = wm.visible_windows();
            let focused = wm.focus.expect("a window stays focused");
            assert!(
                visible.contains(&focused),
                "focused {focused} not visible: {visible:?}"
            );
            assert_eq!(
                visible.first(),
                Some(&focused),
                "focused window must top the z-order"
            );
        }
    }

    /// Part04 4.2.2: `visible_windows` never returns a minimized window —
    /// the renderer's half of "minimized never rendered".
    #[test]
    fn invariant_minimized_never_rendered() {
        let mut wm = wm_with(3);
        let ids: Vec<u64> = wm.windows.iter().map(|w| w.id).collect();
        for id in ids {
            wm.minimize(id);
        }
        assert!(wm.visible_windows().is_empty());
        assert!(wm.focused().is_none(), "nothing left to focus");
    }

    /// Part04 4.2.2: after any operation, every non-maximized window's
    /// rect lies inside the desktop region — clamped, never wrapped.
    #[test]
    fn invariant_rects_within_frame() {
        let mut wm = wm_with(3);
        wm.move_by(1, -500, -500);
        wm.move_by(2, 500, 500);
        wm.resize_edge(3, Edge::Left, -500);
        wm.resize_edge(3, Edge::Right, 500);
        assert_all_within(&wm);
        wm.set_desktop(Rect::new(0, 0, 40, 12));
        assert_all_within(&wm);
    }

    fn assert_all_within(wm: &WmState) {
        for window in &wm.windows {
            assert!(
                rect_within(window.rect, wm.desktop),
                "window {} rect {:?} escaped the desktop {:?}",
                window.id,
                window.rect,
                wm.desktop
            );
        }
    }

    fn rect_within(rect: Rect, bounds: Rect) -> bool {
        rect.x >= bounds.x
            && rect.y >= bounds.y
            && rect.right() <= bounds.right()
            && rect.bottom() <= bounds.bottom()
    }

    /// Part04 4.2.2: ids never repeat — every open hands out a fresh one
    /// and closed ids are never reissued.
    #[test]
    fn invariant_ids_unique() {
        let mut wm = WmState::new(desktop());
        let mut seen = std::collections::HashSet::new();
        for n in 0..50u64 {
            let id = wm.open("w", Some(n as u32));
            assert!(seen.insert(id), "id {id} repeated");
            if n % 3 == 0 {
                wm.close(id);
            }
        }
        let live: Vec<u64> = wm.windows.iter().map(|w| w.id).collect();
        assert_eq!(
            live.len(),
            live.iter().collect::<std::collections::HashSet<_>>().len(),
            "live ids are unique"
        );
    }

    // --------------------------------------------------------- the model

    #[test]
    fn open_cascades_by_two_cells_wrapping() {
        let mut wm = WmState::new(desktop());
        let first = wm.open("a", None);
        let second = wm.open("b", None);
        assert_eq!(
            wm.window(second).expect("b").rect.y,
            wm.window(first).expect("a").rect.y + 2,
            "each window offsets by two cells"
        );
        let mut stepped = WmState::new(desktop());
        let ids: Vec<u64> = (0..14)
            .map(|n| stepped.open(&format!("w{n}"), None))
            .collect();
        let first_rect = stepped.window(ids[0]).expect("first").rect;
        let last_rect = stepped.window(ids[13]).expect("14th").rect;
        assert_eq!(
            last_rect.y,
            first_rect.y + 2,
            "the cascade wraps after {CASCADE_STEPS} steps (open 14 restarts the ladder)"
        );
    }

    #[test]
    fn open_focuses_and_raises() {
        let wm = wm_with(2);
        assert_eq!(wm.focus, Some(2));
        assert_eq!(wm.visible_windows().first(), Some(&2));
    }

    #[test]
    fn close_moves_focus_to_the_next_visible_window() {
        let mut wm = wm_with(3);
        wm.close(3);
        assert_eq!(wm.focus, Some(2), "the next-top window takes focus");
    }

    #[test]
    fn minimize_keeps_the_rect_and_focuses_the_next() {
        let mut wm = wm_with(2);
        let rect = wm.window(2).expect("w2").rect;
        wm.minimize(2);
        let window = wm.window(2).expect("w2 kept");
        assert_eq!(window.state, WinState::Minimized);
        assert_eq!(window.rect, rect, "the rect is kept, cleared nothing");
        assert_eq!(wm.focus, Some(1));
        wm.restore(2);
        let window = wm.window(2).expect("w2");
        assert_eq!(window.state, WinState::Normal);
        assert_eq!(window.rect, rect, "restore returns the exact rect");
        assert_eq!(wm.focus, Some(2));
    }

    #[test]
    fn maximize_fills_the_desktop_and_remembers_the_restore_rect() {
        let mut wm = wm_with(1);
        let before = wm.window(1).expect("w1").rect;
        wm.toggle_maximize(1);
        let window = wm.window(1).expect("w1");
        assert_eq!(window.state, WinState::Maximized);
        assert_eq!(window.restore, before, "the restore rect is remembered");
        assert_eq!(effective_rect(window, wm.desktop), wm.desktop);
        wm.toggle_maximize(1);
        assert_eq!(wm.window(1).expect("w1").rect, before);
        // A shrink while maximized cannot strand the window: the rect is
        // derived from the current desktop.
        wm.toggle_maximize(1);
        wm.set_desktop(Rect::new(0, 0, 40, 10));
        let window = wm.window(1).expect("w1");
        assert_eq!(effective_rect(window, wm.desktop), wm.desktop);
    }

    #[test]
    fn minimize_of_a_maximized_window_keeps_the_restore_rect() {
        let mut wm = wm_with(1);
        let before = wm.window(1).expect("w1").rect;
        wm.toggle_maximize(1);
        wm.minimize(1);
        assert_eq!(wm.window(1).expect("w1").state, WinState::Minimized);
        wm.restore(1);
        assert_eq!(
            wm.window(1).expect("w1").rect,
            before,
            "restore returns to the pre-maximize rect, not the desktop"
        );
    }

    #[test]
    fn move_by_clamps_at_the_desktop_edges() {
        let mut wm = wm_with(1);
        wm.move_by(1, -100, -100);
        assert_eq!(wm.window(1).expect("w1").rect.x, wm.desktop.x);
        assert_eq!(wm.window(1).expect("w1").rect.y, wm.desktop.y);
        wm.move_by(1, 10_000, 10_000);
        let rect = wm.window(1).expect("w1").rect;
        assert_eq!(rect.right(), wm.desktop.right());
        assert_eq!(rect.bottom(), wm.desktop.bottom());
    }

    #[test]
    fn move_and_resize_are_noops_on_maximized_windows() {
        let mut wm = wm_with(1);
        let before = wm.window(1).expect("w1").rect;
        wm.toggle_maximize(1);
        wm.move_by(1, 5, 5);
        wm.resize_edge(1, Edge::Right, 5);
        wm.toggle_maximize(1);
        assert_eq!(
            wm.window(1).expect("w1").rect,
            before,
            "a maximized window neither moves nor resizes"
        );
    }

    #[test]
    fn resize_edge_respects_the_minimum_and_the_desktop() {
        let mut wm = WmState::new(desktop());
        let id = wm.open_at("w", None, Rect::new(10, 5, 30, 12));
        wm.resize_edge(id, Edge::Left, -100);
        let rect = wm.window(id).expect("w").rect;
        assert_eq!(rect.x, wm.desktop.x, "the left edge stops at the desktop");
        assert_eq!(rect.width, 40, "the right edge stays put");
        wm.resize_edge(id, Edge::Right, -100);
        assert_eq!(
            wm.window(id).expect("w").rect.width,
            MIN_WINDOW_WIDTH,
            "never below the minimum"
        );
        wm.resize_edge(id, Edge::Bottom, 100);
        assert_eq!(wm.window(id).expect("w").rect.bottom(), wm.desktop.bottom());
    }

    #[test]
    fn focus_cycling_wraps_and_restores_the_minimized_tail() {
        let mut wm = wm_with(3);
        assert_eq!(wm.focus, Some(3));
        wm.focus_next();
        assert_eq!(wm.focus, Some(1), "next wraps from the last to the first");
        wm.focus_next();
        assert_eq!(wm.focus, Some(2), "next walks the open order");
        wm.focus_next();
        assert_eq!(wm.focus, Some(3));
        wm.focus_prev();
        assert_eq!(wm.focus, Some(2), "prev walks the open order backwards");
        // Cycling must reach every window even though focusing raises.
        let mut round = wm_with(4);
        let mut visited = Vec::new();
        for _ in 0..4 {
            round.focus_next();
            visited.push(round.focus);
        }
        let mut sorted = visited.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, vec![Some(1), Some(2), Some(3), Some(4)]);
        // Minimized windows join the cycle at the tail: visible windows
        // first, then the minimized ones restored on arrival.
        wm.minimize(3);
        wm.minimize(2);
        wm.focus_next();
        assert_eq!(wm.focus, Some(2), "the tail is reached and restored");
        assert_eq!(wm.window(2).expect("w2").state, WinState::Normal);
        wm.focus_next();
        assert_eq!(wm.focus, Some(3), "the next minimized window restores");
        assert_eq!(wm.window(3).expect("w3").state, WinState::Normal);
        wm.focus_next();
        assert_eq!(wm.focus, Some(1), "back to the visible window");
    }

    #[test]
    fn focus_restores_a_minimized_window() {
        let mut wm = wm_with(2);
        wm.minimize(2);
        wm.focus(2);
        assert_eq!(wm.window(2).expect("w2").state, WinState::Normal);
        assert_eq!(wm.visible_windows().first(), Some(&2));
    }

    #[test]
    fn bring_to_front_survives_z_exhaustion() {
        let mut wm = wm_with(2);
        for window in &mut wm.windows {
            window.z = u16::MAX;
        }
        wm.windows[1].z = 1;
        wm.bring_to_front(2);
        assert_eq!(wm.visible_windows().first(), Some(&2));
    }

    #[test]
    fn effective_and_inner_rects() {
        let rect = Rect::new(5, 5, 20, 10);
        assert_eq!(inner_rect(rect), Rect::new(6, 6, 18, 8));
    }

    // -------------------------------------------------------- hit tests

    fn hit(wm: &WmState, col: u16, row: u16) -> Hit {
        hit_test(wm, col, row)
    }

    #[test]
    fn hit_test_zones_of_a_lone_window() {
        let mut wm = WmState::new(desktop());
        let id = wm.open_at("w", None, Rect::new(10, 5, 30, 12));
        let expect = |hit: Hit| assert_eq!(hit.kind_window(), Some(id));
        expect(hit(&wm, 11, 5)); // titlebar (top row, left of buttons)
        expect(hit(&wm, 10, 5)); // top-left corner is titlebar
        expect(hit(&wm, 12, 6)); // interior
        expect(hit(&wm, 10, 6)); // left edge
        expect(hit(&wm, 39, 6)); // right edge
        expect(hit(&wm, 20, 16)); // bottom edge
        expect(hit(&wm, 10, 16)); // bottom-left corner is the bottom edge
        assert_eq!(hit(&wm, 9, 5), Hit::None, "outside the window");
        assert_eq!(hit(&wm, 0, 0), Hit::None, "empty desktop");
    }

    #[test]
    fn hit_test_finds_the_buttons_in_order() {
        let mut wm = WmState::new(desktop());
        let id = wm.open_at("w", None, Rect::new(0, 0, 30, 10));
        // The rendered buttons `- [ #` sit at cols 23/25/28 (right-aligned
        // inside the border); the hit zone starts at col 22 and is
        // forgiving around them.
        assert_eq!(
            hit(&wm, 23, 0),
            Hit::Button {
                window: id,
                button: TitleButton::Minimize
            }
        );
        assert_eq!(
            hit(&wm, 25, 0),
            Hit::Button {
                window: id,
                button: TitleButton::Maximize
            }
        );
        assert_eq!(
            hit(&wm, 28, 0),
            Hit::Button {
                window: id,
                button: TitleButton::Close
            }
        );
        assert_eq!(hit(&wm, 21, 0), Hit::TitleBar { window: id });
    }

    #[test]
    fn hit_test_picks_the_topmost_window() {
        let mut wm = WmState::new(desktop());
        let low = wm.open_at("low", None, Rect::new(0, 0, 30, 10));
        let high = wm.open_at("high", None, Rect::new(4, 2, 30, 10));
        assert_eq!(hit(&wm, 10, 4).kind_window(), Some(high));
        assert_eq!(hit(&wm, 1, 1).kind_window(), Some(low), "uncovered part");
        wm.minimize(high);
        assert_eq!(hit(&wm, 10, 4).kind_window(), Some(low));
    }

    // ------------------------------------------------------ icon grid

    fn grid() -> IconGrid {
        icon_grid(Rect::new(0, 0, 30, 20), 5, 4)
    }

    #[test]
    fn icon_grid_counts_columns_first() {
        let grid = grid();
        assert_eq!((grid.cols, grid.rows), (6, 5), "30/5 columns, 20/4 rows");
        assert_eq!(grid.rect(0), Some(Rect::new(0, 0, 5, 4)), "top-left");
        assert_eq!(
            grid.rect(1),
            Some(Rect::new(0, 4, 5, 4)),
            "column-major: index 1 is *below* index 0"
        );
        assert_eq!(
            grid.rect(5),
            Some(Rect::new(5, 0, 5, 4)),
            "the sixth tile starts the next column"
        );
        assert_eq!(grid.rect(30), None, "past the capacity");
    }

    #[test]
    fn icon_grid_survives_degenerate_inputs() {
        let tiny = icon_grid(Rect::new(0, 0, 3, 3), 5, 4);
        assert_eq!((tiny.cols, tiny.rows), (1, 1), "at least one tile");
        assert_eq!(tiny.count(), 1);
        assert_eq!(tiny.rect(0), Some(Rect::new(0, 0, 5, 4)));
    }

    #[test]
    fn tile_at_finds_the_column_major_index() {
        let grid = grid();
        assert_eq!(grid.tile_at(2, 3), Some(0), "inside tile 0");
        assert_eq!(grid.tile_at(2, 7), Some(1), "one tile down");
        assert_eq!(grid.tile_at(7, 3), Some(5), "one column right");
        assert_eq!(grid.tile_at(30, 3), None, "past the grid's right edge");
        assert_eq!(grid.tile_at(2, 20), None, "past the grid's bottom");
        assert_eq!(grid.tile_at(0, 0), Some(0));
    }

    #[test]
    fn grid_cursor_walks_columns_and_wraps_edges() {
        let grid = grid();
        let count = 17; // a partial last column: 3 full columns + 2 tiles
        let mut cursor = GridCursor::new();
        cursor.move_by(&grid, count, 0, 1);
        assert_eq!(cursor.selected, 1, "down is one tile (column-major)");
        cursor.move_by(&grid, count, 1, 0);
        assert_eq!(cursor.selected, 6, "right is one column");
        cursor.move_by(&grid, count, 0, -1);
        assert_eq!(cursor.selected, 5, "up is one tile back");
        cursor.move_by(&grid, count, -1, 0);
        assert_eq!(cursor.selected, 0, "left is one column back");
        cursor.move_by(&grid, count, -1, 0);
        assert_eq!(
            cursor.selected, 16,
            "left from the first column wraps right, clamped to the last entry"
        );
    }

    #[test]
    fn grid_cursor_wraps_rows_at_a_columns_bottom() {
        let grid = grid();
        let count = 30;
        let mut cursor = GridCursor::new();
        for _ in 0..5 {
            cursor.move_by(&grid, count, 0, 1);
        }
        assert_eq!(
            cursor.selected, 5,
            "down past a column's bottom enters the next"
        );
        cursor.move_by(&grid, count, 0, -1);
        assert_eq!(cursor.selected, 4, "up wraps back into the previous column");
    }

    #[test]
    fn grid_cursor_stays_inside_the_entry_count() {
        let grid = grid();
        let mut cursor = GridCursor::new();
        cursor.move_by(&grid, 2, 1, 0);
        assert_eq!(
            cursor.selected, 1,
            "a column step into empty space clamps to the last entry"
        );
        cursor.move_by(&grid, 0, 0, 1);
        assert_eq!(cursor.selected, 1, "an empty grid never moves");
        cursor.selected = 29;
        cursor.clamp(3);
        assert_eq!(cursor.selected, 2, "clamp follows a shrinking list");
        cursor.clamp(0);
        assert_eq!(cursor.selected, 0);
    }

    // --------------------------------------------------- frame split

    #[test]
    fn frame_regions_gives_taskbar_its_rows_and_desktop_the_remainder() {
        use crate::config::TaskbarPosition;
        let regions = frame_regions(Rect::new(0, 0, 80, 25), 1, TaskbarPosition::Bottom);
        assert_eq!(regions.desktop, Rect::new(0, 0, 80, 24));
        assert_eq!(regions.taskbar, Rect::new(0, 24, 80, 1));
        assert_eq!(regions.overlay, regions.desktop);
    }

    #[test]
    fn frame_regions_top_position_puts_the_taskbar_on_the_first_rows() {
        use crate::config::TaskbarPosition;
        let regions = frame_regions(Rect::new(0, 0, 80, 25), 1, TaskbarPosition::Top);
        assert_eq!(regions.taskbar, Rect::new(0, 0, 80, 1));
        assert_eq!(regions.desktop, Rect::new(0, 1, 80, 24));
        assert_eq!(regions.overlay, regions.desktop);
    }

    /// Audit: the region arithmetic never wraps. A raw struct literal
    /// bypasses ratatui's clamping `Rect::new`, so this is the one way to
    /// hand the core an unfitted rect — the desktop must saturate at the
    /// far edge, never wrap to 0.
    #[test]
    fn frame_regions_survives_a_screen_at_the_u16_edge() {
        use crate::config::TaskbarPosition;
        let area = Rect {
            x: 0,
            y: 65534,
            width: 80,
            height: 2,
        };
        // rows = min(5, 2) = 2, so the desktop has no rows and its y
        // saturates at the far edge; `Rect::new` clamps the expected
        // taskbar's 2 rows to 1 at y=65534.
        let regions = frame_regions(area, 5, TaskbarPosition::Top);
        assert_eq!(regions.taskbar, Rect::new(0, 65534, 80, 1));
        assert_eq!(regions.desktop, Rect::new(0, 65535, 80, 0));
        assert_eq!(regions.overlay, regions.desktop);
        let bottom = frame_regions(area, 5, TaskbarPosition::Bottom);
        assert_eq!(bottom.desktop, Rect::new(0, 65534, 80, 0));
        assert_eq!(bottom.taskbar, Rect::new(0, 65534, 80, 1), "never wraps");
    }

    #[test]
    fn frame_regions_clamps_oversized_taskbars() {
        use crate::config::TaskbarPosition;
        let regions = frame_regions(Rect::new(0, 0, 40, 3), 9, TaskbarPosition::Bottom);
        assert_eq!(regions.desktop.height, 0);
        assert_eq!(regions.taskbar.height, 3);
    }

    impl Hit {
        fn kind_window(&self) -> Option<u64> {
            match *self {
                Hit::TitleBar { window }
                | Hit::Button { window, .. }
                | Hit::Edge { window, .. }
                | Hit::Interior { window } => Some(window),
                Hit::None => None,
            }
        }
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    #[derive(Clone, Copy, Debug)]
    enum WmOp {
        Open,
        OpenAt(Rect),
        Close(u8),
        Focus(u8),
        FocusNext,
        FocusPrev,
        Minimize(u8),
        Restore(u8),
        ToggleMaximize(u8),
        MoveBy(u8, i16, i16),
        ResizeEdge(u8, u8, i16),
        SetDesktop(Rect),
    }

    fn desktop_strategy() -> impl Strategy<Value = Rect> {
        (
            0u16..120,
            0u16..60,
            MIN_WINDOW_WIDTH..220u16,
            MIN_WINDOW_HEIGHT..90u16,
        )
            .prop_map(|(x, y, width, height)| Rect::new(x, y, width, height))
    }

    fn window_rect_strategy() -> impl Strategy<Value = Rect> {
        (
            0u16..150,
            0u16..90,
            MIN_WINDOW_WIDTH..140u16,
            MIN_WINDOW_HEIGHT..70u16,
        )
            .prop_map(|(x, y, width, height)| Rect::new(x, y, width, height))
    }

    fn wm_op_strategy() -> impl Strategy<Value = WmOp> {
        prop_oneof![
            12 => Just(WmOp::Open),
            10 => window_rect_strategy().prop_map(WmOp::OpenAt),
            8 => any::<u8>().prop_map(WmOp::Close),
            8 => any::<u8>().prop_map(WmOp::Focus),
            8 => Just(WmOp::FocusNext),
            8 => Just(WmOp::FocusPrev),
            8 => any::<u8>().prop_map(WmOp::Minimize),
            8 => any::<u8>().prop_map(WmOp::Restore),
            8 => any::<u8>().prop_map(WmOp::ToggleMaximize),
            8 => (any::<u8>(), any::<i16>(), any::<i16>())
                .prop_map(|(pick, dx, dy)| WmOp::MoveBy(pick, dx, dy)),
            8 => (any::<u8>(), 0u8..4, any::<i16>())
                .prop_map(|(pick, edge, delta)| WmOp::ResizeEdge(pick, edge, delta)),
            6 => desktop_strategy().prop_map(WmOp::SetDesktop),
        ]
    }

    fn id_at(wm: &WmState, pick: u8) -> Option<u64> {
        let ids: Vec<u64> = wm.windows.iter().map(|w| w.id).collect();
        if ids.is_empty() {
            return None;
        }
        Some(ids[pick as usize % ids.len()])
    }

    /// Run one operation against the picked window; `Some` carries the
    /// id a fresh `open` handed out.
    fn with_id(wm: &mut WmState, pick: u8, then: impl FnOnce(&mut WmState, u64)) {
        if let Some(id) = id_at(wm, pick) {
            then(wm, id);
        }
    }

    fn apply_op(wm: &mut WmState, op: WmOp) -> Option<u64> {
        match op {
            WmOp::Open => return Some(wm.open("w", None)),
            WmOp::OpenAt(rect) => return Some(wm.open_at("w", None, rect)),
            WmOp::Close(pick) => with_id(wm, pick, |wm, id| wm.close(id)),
            WmOp::Focus(pick) => with_id(wm, pick, |wm, id| wm.focus(id)),
            WmOp::FocusNext => wm.focus_next(),
            WmOp::FocusPrev => wm.focus_prev(),
            WmOp::Minimize(pick) => with_id(wm, pick, |wm, id| wm.minimize(id)),
            WmOp::Restore(pick) => with_id(wm, pick, |wm, id| wm.restore(id)),
            WmOp::ToggleMaximize(pick) => with_id(wm, pick, |wm, id| wm.toggle_maximize(id)),
            WmOp::MoveBy(pick, dx, dy) => {
                with_id(wm, pick, |wm, id| {
                    wm.move_by(id, i32::from(dx), i32::from(dy))
                });
            }
            WmOp::ResizeEdge(pick, edge, delta) => {
                let edge = match edge {
                    0 => Edge::Left,
                    1 => Edge::Right,
                    2 => Edge::Bottom,
                    _ => Edge::Top,
                };
                with_id(wm, pick, |wm, id| {
                    wm.resize_edge(id, edge, i32::from(delta))
                });
            }
            WmOp::SetDesktop(desktop) => wm.set_desktop(desktop),
        }
        None
    }

    fn assert_wm_sane(wm: &WmState) {
        for window in &wm.windows {
            if window.state == WinState::Maximized {
                continue;
            }
            assert!(
                rect_within(window.rect, wm.desktop),
                "window {} rect {:?} escaped the desktop {:?}",
                window.id,
                window.rect,
                wm.desktop
            );
        }
        let visible = wm.visible_windows();
        let Some(focused) = wm.focus else {
            return;
        };
        let window = wm.window(focused).expect("focus names a live window");
        assert_ne!(
            window.state,
            WinState::Minimized,
            "focus {focused} is minimized"
        );
        assert!(
            visible.contains(&focused),
            "focused {focused} not in {visible:?}"
        );
        assert_eq!(
            visible.first(),
            Some(&focused),
            "focused {focused} not on top of {visible:?}"
        );
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// The Part04 invariants hold after every prefix of every random
        /// operation sequence: focus is visible and on top, rects stay
        /// inside the desktop, focus is never a minimized window.
        /// (`bring_to_front` is excluded — raising without focusing is
        /// its documented contract; see the `WmState` doc.)
        #[test]
        fn invariant_random_op_sequences_keep_the_wm_sane(
            desktop in desktop_strategy(),
            ops in proptest::collection::vec(wm_op_strategy(), 0..50),
        ) {
            let mut wm = WmState::new(desktop);
            for op in ops {
                apply_op(&mut wm, op);
                assert_wm_sane(&wm);
            }
        }

        /// Every id ever handed out stays unique for the whole stream —
        /// closed ids are never reissued.
        #[test]
        fn invariant_random_open_close_streams_never_repeat_ids(
            ops in proptest::collection::vec(wm_op_strategy(), 0..50),
        ) {
            let mut wm = WmState::new(desktop());
            let mut seen = std::collections::HashSet::new();
            for op in ops {
                if let Some(id) = apply_op(&mut wm, op) {
                    assert!(seen.insert(id), "id {id} was reissued");
                }
            }
        }

        /// The frame split tiles the screen at every size, row count and
        /// taskbar position; the overlay is always the desktop region.
        #[test]
        fn prop_frame_regions_tile_the_screen(
            origin in (0..60_000u16, 0..60_000u16),
            shape in (1..300u16, 1..100u16),
            rows in 0..30u16,
            top in any::<bool>(),
        ) {
            use crate::config::TaskbarPosition;
            let position = if top {
                TaskbarPosition::Top
            } else {
                TaskbarPosition::Bottom
            };
            let area = Rect::new(origin.0, origin.1, shape.0, shape.1);
            let regions = frame_regions(area, rows, position);
            prop_assert_eq!(regions.taskbar.width, area.width);
            prop_assert_eq!(regions.desktop.width, area.width);
            prop_assert_eq!(regions.taskbar.height, rows.min(area.height));
            prop_assert_eq!(
                u32::from(regions.taskbar.height) + u32::from(regions.desktop.height),
                u32::from(area.height)
            );
            prop_assert_eq!(regions.overlay, regions.desktop);
            if top {
                prop_assert_eq!(regions.taskbar.y, area.y, "a top taskbar starts at the top");
                prop_assert_eq!(regions.desktop.y, area.y + regions.taskbar.height);
            } else {
                prop_assert_eq!(regions.desktop.y, area.y);
                prop_assert_eq!(
                    u32::from(regions.taskbar.y) + u32::from(regions.taskbar.height),
                    u32::from(area.y) + u32::from(area.height),
                    "a bottom taskbar runs to the bottom edge"
                );
            }
        }

        /// Every grid tile's top-left maps back to its index.
        #[test]
        fn prop_icon_grid_tile_at_inverts_rect(
            origin in (0..200u16, 0..200u16),
            shape in (1..300u16, 1..200u16),
            tile in (1..40u16, 1..40u16),
        ) {
            let grid = icon_grid(Rect::new(origin.0, origin.1, shape.0, shape.1), tile.0, tile.1);
            prop_assert_eq!(grid.count(), grid.cols as usize * grid.rows as usize);
            for index in 0..grid.count() {
                let rect = grid.rect(index).expect("index below capacity");
                prop_assert_eq!(grid.tile_at(rect.x, rect.y), Some(index), "tile {} lost", index);
            }
        }

        /// The cursor never leaves the entry list, whatever the moves.
        #[test]
        fn prop_grid_cursor_stays_inside_the_count(
            origin in (0..50u16, 0..50u16),
            shape in (1..80u16, 1..60u16),
            tile in (1..12u16, 1..12u16),
            count in 0..80usize,
            moves in proptest::collection::vec((any::<i32>(), any::<i32>()), 0..30),
        ) {
            let grid = icon_grid(Rect::new(origin.0, origin.1, shape.0, shape.1), tile.0, tile.1);
            let mut cursor = GridCursor::new();
            cursor.clamp(count);
            for (columns, rows) in moves {
                cursor.move_by(&grid, count, columns, rows);
                prop_assert!(
                    cursor.selected <= count.saturating_sub(1),
                    "selected {} past count {}",
                    cursor.selected,
                    count
                );
            }
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    /// The independent hit-test table: zone by cell position alone,
    /// for a lone window whose id is always 1 on a fresh state.
    fn expected_hit(rect: Rect, col: u16, row: u16) -> Hit {
        let inside = col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height;
        if !inside {
            return Hit::None;
        }
        if row == rect.y {
            let start = rect.right() - 1 - TITLE_BUTTON_COLS;
            if col < start {
                return Hit::TitleBar { window: 1 };
            }
            return Hit::Button {
                window: 1,
                button: button_at(col - start),
            };
        }
        if row == rect.bottom() - 1 {
            return Hit::Edge {
                window: 1,
                edge: Edge::Bottom,
            };
        }
        if col == rect.x {
            return Hit::Edge {
                window: 1,
                edge: Edge::Left,
            };
        }
        if col == rect.right() - 1 {
            return Hit::Edge {
                window: 1,
                edge: Edge::Right,
            };
        }
        Hit::Interior { window: 1 }
    }

    /// The documented edge semantics, written out independently: move
    /// one edge by `delta` (positive grows), keep the opposite edge, the
    /// minimum size and the desktop bounds.
    fn expected_resize(rect: Rect, edge: Edge, delta: i32, desktop: Rect) -> Rect {
        match edge {
            Edge::Left => {
                let max_x = rect.right().saturating_sub(MIN_WINDOW_WIDTH);
                let x = clamp_u16(clamp_i32(
                    rect.x as i32 + delta,
                    desktop.x as i32,
                    max_x as i32,
                ));
                Rect::new(x, rect.y, rect.right() - x, rect.height)
            }
            Edge::Right => {
                let max_w = desktop.right().saturating_sub(rect.x).max(MIN_WINDOW_WIDTH);
                let w = clamp_u16(clamp_i32(
                    rect.width as i32 + delta,
                    MIN_WINDOW_WIDTH as i32,
                    max_w as i32,
                ));
                Rect::new(rect.x, rect.y, w, rect.height)
            }
            Edge::Top => {
                let max_y = rect.bottom().saturating_sub(MIN_WINDOW_HEIGHT);
                let y = clamp_u16(clamp_i32(
                    rect.y as i32 + delta,
                    desktop.y as i32,
                    max_y as i32,
                ));
                Rect::new(rect.x, y, rect.width, rect.bottom() - y)
            }
            Edge::Bottom => {
                let max_h = desktop
                    .bottom()
                    .saturating_sub(rect.y)
                    .max(MIN_WINDOW_HEIGHT);
                let h = clamp_u16(clamp_i32(
                    rect.height as i32 + delta,
                    MIN_WINDOW_HEIGHT as i32,
                    max_h as i32,
                ));
                Rect::new(rect.x, rect.y, rect.width, h)
            }
        }
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// Every desktop cell classifies exactly as position arithmetic
        /// says, for a lone randomly-placed window.
        #[test]
        fn prop_hit_test_matches_the_position_table(
            origin in (0..40u16, 0..10u16),
            shape in (MIN_WINDOW_WIDTH..40u16, MIN_WINDOW_HEIGHT..12u16),
        ) {
            let mut wm = WmState::new(desktop());
            let id = wm.open_at("w", None, Rect::new(origin.0, origin.1, shape.0, shape.1));
            let rect = wm.window(id).expect("w").rect;
            let area = desktop();
            let cells = (0..area.height)
                .flat_map(move |row| (0..area.width).map(move |col| (col, row)));
            for (col, row) in cells {
                let got = hit_test(&wm, col, row);
                prop_assert_eq!(
                    got,
                    expected_hit(rect, col, row),
                    "cell ({}, {})",
                    col,
                    row
                );
            }
        }

        /// resize_edge is exactly the documented edge arithmetic.
        #[test]
        fn prop_resize_edge_keeps_minimum_bounds_and_fixed_edges(
            origin in (0..30u16, 0..10u16),
            shape in (MIN_WINDOW_WIDTH..40u16, MIN_WINDOW_HEIGHT..12u16),
            edge_pick in 0u8..4,
            delta in -500i32..500,
        ) {
            let mut wm = WmState::new(desktop());
            let id = wm.open_at("w", None, Rect::new(origin.0, origin.1, shape.0, shape.1));
            let before = wm.window(id).expect("w").rect;
            let edge = match edge_pick {
                0 => Edge::Left,
                1 => Edge::Right,
                2 => Edge::Top,
                _ => Edge::Bottom,
            };
            wm.resize_edge(id, edge, delta);
            let after = wm.window(id).expect("w").rect;
            prop_assert!(
                after.width >= MIN_WINDOW_WIDTH && after.height >= MIN_WINDOW_HEIGHT,
                "shrank below the minimum: {:?}",
                after
            );
            prop_assert!(
                rect_within(after, wm.desktop),
                "escaped the desktop: {:?}",
                after
            );
            prop_assert_eq!(after, expected_resize(before, edge, delta, wm.desktop));
        }

        /// open() sizes a window at three quarters of the desktop and
        /// cascades by two cells from the desktop's origin.
        #[test]
        fn prop_open_windows_are_three_quarters_and_cascade(
            desktop in desktop_strategy(),
            opens in 1..8usize,
        ) {
            let mut wm = WmState::new(desktop);
            let ids: Vec<u64> = (0..opens).map(|_| wm.open("w", None)).collect();
            for (index, id) in ids.into_iter().enumerate() {
                let rect = wm.window(id).expect("w").rect;
                let step = ((index % 12) * 2) as u16;
                let want_w = (desktop.width * 3 / 4)
                    .max(MIN_WINDOW_WIDTH)
                    .min(desktop.width.max(MIN_WINDOW_WIDTH));
                let want_h = (desktop.height * 3 / 4)
                    .max(MIN_WINDOW_HEIGHT)
                    .min(desktop.height.max(MIN_WINDOW_HEIGHT));
                prop_assert_eq!(rect.width, want_w, "width of open {}", index);
                prop_assert_eq!(rect.height, want_h, "height of open {}", index);
                prop_assert_eq!(
                    rect.x,
                    desktop.x + step.min(desktop.width.saturating_sub(want_w)),
                    "x of open {}",
                    index
                );
                prop_assert_eq!(
                    rect.y,
                    desktop.y + step.min(desktop.height.saturating_sub(want_h)),
                    "y of open {}",
                    index
                );
            }
        }
    }

    #[test]
    fn set_desktop_updates_the_region_and_reclamps() {
        let mut wm = wm_with(2);
        wm.set_desktop(Rect::new(0, 0, 40, 12));
        assert_eq!(
            wm.desktop,
            Rect::new(0, 0, 40, 12),
            "the region must update"
        );
        assert_all_within(&wm);
        wm.set_desktop(Rect::new(10, 5, 40, 12));
        for window in &wm.windows {
            assert!(
                rect_within(window.rect, wm.desktop),
                "rects follow the new origin"
            );
        }
    }

    #[test]
    fn pane_title_focus_and_order_accessors() {
        let mut wm = WmState::new(desktop());
        let a = wm.open("a", None);
        let b = wm.open_at("b", None, Rect::new(0, 0, 20, 10));
        wm.set_pane(a, Some(7));
        assert_eq!(wm.window(a).expect("a").pane_id, Some(7));
        wm.set_pane(a, None);
        assert_eq!(wm.window(a).expect("a").pane_id, None);
        wm.set_title(a, "renamed");
        assert_eq!(wm.window(a).expect("a").title, "renamed");
        wm.minimize(a);
        assert_eq!(
            wm.focused().map(|w| w.id),
            Some(b),
            "focused() sees the focused window"
        );
        assert_eq!(
            wm.focus_order(),
            vec![b, a],
            "minimized stay in the cycle order"
        );
        assert_eq!(wm.visible_windows(), vec![b]);
    }

    #[test]
    fn move_by_only_touches_the_named_window() {
        let mut wm = wm_with(2);
        let rects: Vec<Rect> = wm.windows.iter().map(|w| w.rect).collect();
        wm.move_by(2, 4, 4);
        assert_eq!(wm.window(2).expect("w2").rect.x, rects[1].x + 4, "w2 moves");
        assert_eq!(wm.window(1).expect("w1").rect, rects[0], "w1 stays put");
        wm.move_by(1, 4, 4);
        assert_eq!(
            wm.window(1).expect("w1").rect.x,
            rects[0].x + 4,
            "w1 moves too"
        );
    }

    #[test]
    fn minimize_keeps_the_moved_rect() {
        let mut wm = wm_with(1);
        wm.move_by(1, 6, 6);
        let moved = wm.window(1).expect("w1").rect;
        wm.minimize(1);
        assert_eq!(
            wm.window(1).expect("w1").rect,
            moved,
            "minimize clears nothing"
        );
        wm.restore(1);
        assert_eq!(
            wm.window(1).expect("w1").rect,
            moved,
            "restore returns the moved rect"
        );
    }

    #[test]
    fn restore_of_a_maximized_window_only_focuses() {
        let mut wm = wm_with(1);
        wm.toggle_maximize(1);
        wm.restore(1);
        assert_eq!(
            wm.window(1).expect("w1").state,
            WinState::Maximized,
            "restore() is not un-maximize"
        );
    }

    #[test]
    fn bring_to_front_normalization_preserves_the_order() {
        let mut wm = wm_with(3);
        // True z-order 1 > 2 > 3 (not the insertion order); exhausting
        // z forces a normalization on the next raise, which must
        // re-space without reordering.
        wm.windows[0].z = u16::MAX;
        wm.windows[1].z = u16::MAX - 1;
        wm.windows[2].z = u16::MAX - 2;
        wm.bring_to_front(3);
        assert_eq!(
            wm.visible_windows(),
            vec![3, 1, 2],
            "3 on top, the 1-over-2 order preserved"
        );
    }
}
