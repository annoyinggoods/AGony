//! The file panel (Part05 5.3–5.5): an internal window (no pane — the
//! panel renders a widget) showing the ignore-aware tree of a root, with
//! the terminal-native navigation quartet, the `/` search overlay over
//! the snapshot, and the grab (drag source) that Part07's command bar
//! drops into.
//!
//! Walking is a worker (`fsprobe`); rendering is a snapshot read — the
//! panel never blocks on IO. The tree state (cursor, expanded set) is
//! core-side (`agony_core::tree`); search ranking is the one engine
//! (`suggest::rank_paths`, with matched characters highlighted).
//!
//! Items: `State` — the panel's whole state (root, shared snapshot,
//! cursor, scroll offset, search overlay); `Search` — query, window
//! offset, selection; `Event` — what a key press asked the app to do
//! (`OpenFile`, `Grab`, `OpenSearch`, `Refresh`, `DropPick`, `Consumed`);
//! `View` — one draw's inputs.
//! Core functions: `open` — create the state and start the first walk;
//! `State::refresh` — restart the walk under fresh options; `handle_key`
//! — the pure routing; `scroll_by` / `page` / `clamp_scroll` — the one
//! viewport offset the wheel (Part06) and PageUp/PageDown move, clamped
//! on every content change; `selected_path` — the selected node's
//! absolute path; `render` — the widget (tree rows or search results,
//! footer).
//! Depends on: agony_core::tree (model), suggest (ranking, highlights),
//! pathpick (`Picked`), icon (folder glyph), fsprobe (the walker),
//! theme.
//! Called by: app (lifecycle, keys, draws), wm (renders it into its
//! window).
//! Calls: fsprobe's shared snapshot only — no direct IO here.

use std::path::PathBuf;

use agony_core::config::FontMode;
use agony_core::layout::Rect;
use agony_core::pathpick::Picked;
use agony_core::suggest::{self, PathItem};
use agony_core::tree::{node_at, TreeCursor};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::fsprobe;
use crate::theme::{Applier, Role};
use crate::wm;

/// The `/` overlay: a query over the snapshot, one window of results at
/// a time (`n` pages; the window is the overlay's height).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    pub offset: usize,
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Consumed,
    /// Enter on a file: open it with the manifest whose `open_with`
    /// covers its extension (the app resolves the app).
    OpenFile {
        path: PathBuf,
    },
    /// `g` on a file: pick it up (the drag source).
    Grab {
        path: PathBuf,
    },
    /// `/`: open the search overlay (the app refreshes a stale snapshot
    /// once, then typing filters).
    OpenSearch,
    /// `r`: refresh the walk.
    Refresh,
    /// Esc with a file held: put it down.
    DropPick,
}

pub struct State {
    pub root: PathBuf,
    tree: fsprobe::SharedTree,
    pub cursor: TreeCursor,
    /// The first visible tree row (Part06 6.3.3): keys and the wheel
    /// move the same offset, clamped to the content on every change.
    pub scroll: usize,
    pub search: Option<Search>,
    /// The last drawn result-window height (`n` pages by it; the clamp
    /// and the keys need it before the next draw).
    pub window: usize,
}

/// Open the panel on a root: the state plus the first walk, already
/// running on the worker.
pub fn open(root: PathBuf, options: fsprobe::Options) -> State {
    let tree = fsprobe::start_walk(root.clone(), options);
    State {
        root,
        tree,
        cursor: TreeCursor::new(),
        scroll: 0,
        search: None,
        window: 10,
    }
}

impl State {
    /// Restart the walk under fresh options (a settings toggle or `r`).
    pub fn refresh(&mut self, options: fsprobe::Options) {
        self.tree = fsprobe::start_walk(self.root.clone(), options);
        self.clamp_scroll();
    }

    /// A copy of the current roots (startup's first expansion and the
    /// tests' seam).
    pub fn roots_snapshot(&self) -> Vec<agony_core::tree::TreeNode> {
        fsprobe::lock(&self.tree).roots.clone()
    }

    fn roots(&self) -> Vec<agony_core::tree::TreeNode> {
        self.roots_snapshot()
    }

    /// Wheel scroll (Part06 6.3.2): signed rows (positive = later
    /// content), then the clamp keeps the cursor in view.
    pub fn scroll_by(&mut self, rows: i32) {
        self.scroll = shift_offset(self.scroll, rows);
        self.clamp_scroll();
    }

    /// PageUp/PageDown: the same offset, one window at a time.
    pub fn page(&mut self, direction: i32) {
        self.scroll_by(direction * self.window.max(1) as i32);
    }

    /// The one clamp (Part06 6.3.3's pitfall row): the offset stays
    /// inside the visible rows and the cursor stays inside the window —
    /// a scroll that would strand the cursor pulls it to the nearest
    /// visible row instead. Called on every offset, cursor, expansion
    /// and content change.
    pub fn clamp_scroll(&mut self) {
        let rows = self.cursor.visible(&self.roots());
        let visible = self.window.max(1);
        self.scroll = self.scroll.min(rows.len().saturating_sub(visible));
        let Some(at) = rows
            .iter()
            .position(|row| row.path == self.cursor.selected())
        else {
            return;
        };
        if at >= self.scroll && at < self.scroll + visible {
            return;
        }
        let target = if at < self.scroll {
            self.scroll
        } else {
            (self.scroll + visible).saturating_sub(1)
        };
        if let Some(row) = rows.get(target) {
            self.cursor.select(&row.path);
        }
    }
}

/// Shift an offset by signed rows, saturating at zero (the upper clamp
/// is content-dependent and lives in `clamp_scroll`).
fn shift_offset(offset: usize, rows: i32) -> usize {
    if rows >= 0 {
        offset.saturating_add(rows as usize)
    } else {
        offset.saturating_sub(rows.unsigned_abs() as usize)
    }
}

/// The selected node's absolute path (root + the labels along the cursor
/// path), or None when nothing resolves.
pub fn selected_path(state: &State) -> Option<PathBuf> {
    let roots = state.roots();
    let path = state.cursor.selected();
    if path.is_empty() {
        return None;
    }
    let mut full = state.root.clone();
    for depth in 0..path.len() {
        full.push(label_at(&roots, &path[..=depth])?);
    }
    Some(full)
}

fn label_at(roots: &[agony_core::tree::TreeNode], path: &[usize]) -> Option<String> {
    node_at(roots, path).map(|node| node.label.clone())
}

/// One key press, routed by the overlay's state. Nothing here awaits —
/// the events are the app's to act on.
pub fn handle_key(
    state: &mut State,
    key: crossterm::event::KeyEvent,
    picked: Option<&Picked>,
) -> Event {
    let plain = key.modifiers.is_empty() || key.modifiers == crossterm::event::KeyModifiers::SHIFT;
    let code = key.code;
    if state.search.is_some() {
        return search_key(state, code, plain);
    }
    match code {
        crossterm::event::KeyCode::Up if plain => {
            move_cursor(state, -1);
            Event::Consumed
        }
        crossterm::event::KeyCode::Down if plain => {
            move_cursor(state, 1);
            Event::Consumed
        }
        crossterm::event::KeyCode::PageUp if plain => {
            state.page(-1);
            Event::Consumed
        }
        crossterm::event::KeyCode::PageDown if plain => {
            state.page(1);
            Event::Consumed
        }
        crossterm::event::KeyCode::Left if plain => {
            state.cursor.move_left();
            state.clamp_scroll();
            Event::Consumed
        }
        crossterm::event::KeyCode::Right if plain => {
            expand_selected(state);
            Event::Consumed
        }
        crossterm::event::KeyCode::Enter if plain => open_selected(state),
        crossterm::event::KeyCode::Char('/') if plain => Event::OpenSearch,
        crossterm::event::KeyCode::Char('r') if plain => Event::Refresh,
        crossterm::event::KeyCode::Char('g') if plain => grab_selected(state),
        crossterm::event::KeyCode::Esc if picked.is_some() => Event::DropPick,
        crossterm::event::KeyCode::Esc => Event::Consumed,
        _ => Event::Consumed,
    }
}

fn move_cursor(state: &mut State, direction: i32) {
    let roots = state.roots();
    match direction {
        positive if positive > 0 => state.cursor.move_down(&roots),
        _ => state.cursor.move_up(&roots),
    }
    state.clamp_scroll();
}

fn expand_selected(state: &mut State) {
    let roots = state.roots();
    state.cursor.move_right(&roots);
    state.clamp_scroll();
}

/// Enter: a file opens (the app's `open_with` lookup); a dir expands
/// inline — the tree grows, it does not navigate away (right and Enter
/// both open a folder).
fn open_selected(state: &mut State) -> Event {
    let Some(path) = selected_path(state) else {
        return Event::Consumed;
    };
    let roots = state.roots();
    match node_at(&roots, state.cursor.selected()) {
        Some(node) if node.leaf => Event::OpenFile { path },
        _ => {
            state.cursor.move_right(&roots);
            state.clamp_scroll();
            Event::Consumed
        }
    }
}

fn grab_selected(state: &State) -> Event {
    let Some(path) = selected_path(state) else {
        return Event::Consumed;
    };
    let roots = state.roots();
    match node_at(&roots, state.cursor.selected()) {
        Some(node) if node.leaf => Event::Grab { path },
        _ => Event::Consumed,
    }
}

fn search_key(state: &mut State, code: crossterm::event::KeyCode, plain: bool) -> Event {
    if code == crossterm::event::KeyCode::Esc {
        state.search = None;
        return Event::Consumed;
    }
    if code == crossterm::event::KeyCode::Enter && plain {
        return open_search_result(state);
    }
    let results = search_results(state);
    let Some(search) = state.search.as_mut() else {
        return Event::Consumed;
    };
    let shown = results
        .len()
        .saturating_sub(search.offset)
        .min(state.window);
    match code {
        crossterm::event::KeyCode::Backspace if plain => {
            search.query.pop();
            search.offset = 0;
            search.selected = 0;
            Event::Consumed
        }
        crossterm::event::KeyCode::Up if plain => {
            search.selected = search.selected.saturating_sub(1);
            Event::Consumed
        }
        crossterm::event::KeyCode::Down if plain => {
            search.selected = (search.selected + 1).min(shown.saturating_sub(1));
            Event::Consumed
        }
        crossterm::event::KeyCode::Char('n') if plain => {
            search.offset += state.window.max(1);
            search.selected = 0;
            Event::Consumed
        }
        crossterm::event::KeyCode::Char(ch) if plain => {
            search.query.push(ch);
            search.offset = 0;
            search.selected = 0;
            Event::Consumed
        }
        _ => Event::Consumed,
    }
}

/// Enter in the overlay: open the selected result and close the overlay.
fn open_search_result(state: &mut State) -> Event {
    let results = search_results(state);
    let Some(search) = &state.search else {
        return Event::Consumed;
    };
    let Some(hit) = results.get(search.offset + search.selected) else {
        return Event::Consumed;
    };
    let path = state.root.join(hit.hint.trim_end_matches(['\\', '/']));
    state.search = None;
    Event::OpenFile { path }
}

/// The ranked results for the current query over the current snapshot.
pub fn search_results(state: &State) -> Vec<suggest::Suggestion> {
    let Some(search) = &state.search else {
        return Vec::new();
    };
    if search.query.is_empty() {
        return Vec::new();
    }
    let corpus = corpus(&state.roots());
    suggest::rank_paths(&search.query, &corpus)
}

fn corpus(roots: &[agony_core::tree::TreeNode]) -> Vec<PathItem> {
    agony_core::tree::flatten_paths(roots, "\\")
        .into_iter()
        .map(|path| PathItem {
            label: agony_core::pathpick::file_name(path.trim_end_matches(['\\', '/'])).to_string(),
            path,
        })
        .collect()
}

/// The docked-left default rect (Part05 5.3.1): a third of the desktop
/// wide (capped), full height — a default only; after open it is a
/// normal window.
pub fn docked_rect(desktop: Rect) -> Rect {
    let width = (desktop.width / 3).clamp(12, 40).min(desktop.width);
    Rect::new(desktop.x, desktop.y, width, desktop.height)
}

// ----------------------------------------------------------------- render

pub struct View<'a> {
    pub state: &'a mut State,
    pub picked: Option<&'a Picked>,
}

pub fn render(
    frame: &mut Frame<'_>,
    inner: Rect,
    view: &mut View<'_>,
    focused: bool,
    theme: &Applier,
    font: FontMode,
) {
    let state = &mut *view.state;
    let (body, footer) = split(inner);
    if state.search.is_some() {
        frame.render_widget(search_paragraph(state, body.height, theme), body);
    } else {
        state.window = body.height as usize;
        state.clamp_scroll();
        frame.render_widget(
            tree_paragraph(state, body.height, focused, theme, font),
            body,
        );
    }
    frame.render_widget(footer_paragraph(view.state, view.picked, theme), footer);
}

fn split(inner: Rect) -> (Rect, Rect) {
    let footer_h = inner.height.min(1);
    let body_h = inner.height - footer_h;
    (
        Rect::new(inner.x, inner.y, inner.width, body_h),
        Rect::new(inner.x, inner.y + body_h, inner.width, footer_h),
    )
}

fn tree_paragraph(
    state: &State,
    height: u16,
    focused: bool,
    theme: &Applier,
    font: FontMode,
) -> Paragraph<'static> {
    let snapshot = fsprobe::lock(&state.tree);
    if snapshot.roots.is_empty() {
        let text = match &snapshot.error {
            Some(error) => format!("walk failed: {error}"),
            None => "walking…".to_string(),
        };
        return Paragraph::new(text).style(theme.style(Role::TextDim, Role::Background));
    }
    let rows = state.cursor.visible(&snapshot.roots);
    let selected = state.cursor.selected().to_vec();
    let lines: Vec<Line<'static>> = rows
        .iter()
        .skip(state.scroll)
        .take(height.max(1) as usize)
        .map(|row| tree_line(row, selected == row.path, focused, theme, font))
        .collect();
    Paragraph::new(lines).style(theme.style(Role::Text, Role::Background))
}

fn tree_line(
    row: &agony_core::tree::VisibleRow,
    selected: bool,
    focused: bool,
    theme: &Applier,
    font: FontMode,
) -> Line<'static> {
    let marker = if row.leaf {
        " "
    } else {
        wm::folder_glyph(font)
    };
    let indent = "  ".repeat(row.depth);
    let text = format!("{indent}{marker} {}", row.label);
    let style = match (selected, focused) {
        (true, true) => theme.style(Role::Background, Role::Accent),
        (true, false) => theme.style(Role::Accent, Role::Background),
        _ => theme.style(Role::Text, Role::Background),
    };
    Line::from(text).style(style)
}

fn search_paragraph(state: &State, height: u16, theme: &Applier) -> Paragraph<'static> {
    let Some(search) = &state.search else {
        return Paragraph::new("");
    };
    let results = search_results(state);
    let mut lines = vec![Line::from(format!("/{}_", search.query))
        .style(theme.style(Role::Accent, Role::Background))];
    if results.is_empty() {
        lines.push(Line::from("(no matches)").style(theme.style(Role::TextDim, Role::Background)));
    }
    let window = (height.max(1) as usize).saturating_sub(1);
    lines.extend(
        results
            .iter()
            .skip(search.offset)
            .take(window)
            .enumerate()
            .map(|(at, hit)| search_line(&search.query, hit, at == search.selected, theme)),
    );
    Paragraph::new(lines).style(theme.style(Role::Text, Role::Background))
}

fn search_line(
    query: &str,
    hit: &suggest::Suggestion,
    selected: bool,
    theme: &Applier,
) -> Line<'static> {
    let base = if selected {
        theme.style(Role::Background, Role::Accent)
    } else {
        theme.style(Role::Text, Role::Background)
    };
    let Some(positions) = suggest::match_positions(query, &hit.hint) else {
        return Line::from(hit.hint.clone()).style(base);
    };
    let mut spans = Vec::new();
    for (at, ch) in hit.hint.chars().enumerate() {
        let matched = positions.contains(&at);
        let style = if matched {
            theme.style(Role::Accent, Role::Background)
        } else {
            base
        };
        spans.push(Span::styled(ch.to_string(), style));
    }
    Line::from(spans)
}

fn footer_paragraph(state: &State, picked: Option<&Picked>, theme: &Applier) -> Paragraph<'static> {
    let truncated = fsprobe::lock(&state.tree).truncated;
    let held = picked.map(|_| " ·held").unwrap_or_default();
    let mut text = format!("r refresh · / find · g grab{held}");
    if truncated {
        text.push_str(" · truncated");
    }
    Paragraph::new(text).style(theme.style(Role::TextDim, Role::Background))
}

/// The window title the app hands the wm (root path, held marker).
pub fn window_title(state: &State, picked: Option<&Picked>) -> String {
    let held = picked.map(|_| " —held").unwrap_or_default();
    format!("files: {}{}", state.root.display(), held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agony_core::tree::TreeNode;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// The fixture root is Windows-flavored data ("C:\work"); the
    /// expected child paths are built through `join` so the tests hold
    /// on every platform's separator (the 2026-10-27 Linux CI finding).
    fn work_root() -> PathBuf {
        PathBuf::from("C:\\work")
    }

    fn state_with(roots: Vec<TreeNode>) -> State {
        let shared: fsprobe::SharedTree =
            std::sync::Arc::new(std::sync::Mutex::new(fsprobe::Snapshot {
                roots,
                truncated: false,
                error: None,
            }));
        State {
            root: work_root(),
            tree: shared,
            cursor: TreeCursor::new(),
            scroll: 0,
            search: None,
            window: 10,
        }
    }

    fn tree() -> Vec<TreeNode> {
        let mut src = TreeNode::dir("src");
        src.children.push(TreeNode::file("main.rs"));
        vec![src, TreeNode::file("readme.md")]
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn key_char(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty())
    }

    #[test]
    fn navigation_moves_expand_and_open() {
        let mut state = state_with(tree());
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Down), None),
            Event::Consumed
        );
        assert_eq!(state.cursor.selected(), &[1], "down moves to readme.md");
        handle_key(&mut state, key(KeyCode::Up), None);
        assert_eq!(state.cursor.selected(), &[0]);
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Right), None),
            Event::Consumed,
            "right expands a dir (no event)"
        );
        assert!(state.cursor.is_expanded(&[0]));
        handle_key(&mut state, key(KeyCode::Down), None);
        assert_eq!(state.cursor.selected(), &[0, 0], "into src/main.rs");
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter), None),
            Event::OpenFile {
                path: work_root().join("src").join("main.rs")
            }
        );
    }

    #[test]
    fn enter_on_a_dir_expands_instead_of_opening() {
        let mut state = state_with(tree());
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter), None),
            Event::Consumed
        );
        assert!(state.cursor.is_expanded(&[0]));
    }

    #[test]
    fn grab_takes_files_only() {
        let mut state = state_with(tree());
        assert_eq!(
            handle_key(&mut state, key_char('g'), None),
            Event::Consumed,
            "g on a dir picks nothing up"
        );
    }

    #[test]
    fn grab_works_on_files() {
        let mut state = state_with(tree());
        handle_key(&mut state, key(KeyCode::Right), None);
        handle_key(&mut state, key(KeyCode::Down), None); // main.rs
        assert_eq!(
            handle_key(&mut state, key_char('g'), None),
            Event::Grab {
                path: work_root().join("src").join("main.rs")
            }
        );
    }

    #[test]
    fn search_overlay_types_and_opens_a_result() {
        let mut state = state_with(tree());
        handle_key(&mut state, key_char('/'), None); // app opens it; simulate
        state.search = Some(Search::default());
        for ch in "readme".chars() {
            handle_key(&mut state, key_char(ch), None);
        }
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter), None),
            Event::OpenFile {
                path: work_root().join("readme.md")
            }
        );
        assert!(state.search.is_none(), "enter closes the overlay");
    }

    #[test]
    fn search_escape_and_backspace() {
        let mut state = state_with(tree());
        state.search = Some(Search::default());
        handle_key(&mut state, key_char('m'), None);
        handle_key(&mut state, key(KeyCode::Backspace), None);
        assert_eq!(state.search.as_ref().expect("open").query, "");
        handle_key(&mut state, key(KeyCode::Esc), None);
        assert!(state.search.is_none());
    }

    #[test]
    fn escape_with_a_pick_drops_it() {
        let mut state = state_with(tree());
        let picked = Picked {
            path: r"C:\work\readme.md".into(),
        };
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Esc), Some(&picked)),
            Event::DropPick
        );
    }

    #[test]
    fn slash_and_r_raise_events() {
        let mut state = state_with(tree());
        assert_eq!(
            handle_key(&mut state, key_char('/'), None),
            Event::OpenSearch
        );
        assert_eq!(handle_key(&mut state, key_char('r'), None), Event::Refresh);
    }

    #[test]
    fn docked_rect_is_a_third_capped() {
        assert_eq!(
            docked_rect(Rect::new(0, 0, 90, 20)),
            Rect::new(0, 0, 30, 20)
        );
        assert_eq!(docked_rect(Rect::new(0, 0, 200, 20)).width, 40);
        assert_eq!(docked_rect(Rect::new(0, 0, 20, 10)).width, 12);
    }

    #[test]
    fn window_title_carries_the_held_marker() {
        let state = state_with(tree());
        assert_eq!(window_title(&state, None), "files: C:\\work");
        let picked = Picked { path: "x".into() };
        assert!(window_title(&state, Some(&picked)).ends_with("—held"));
    }

    #[test]
    fn selected_path_resolves_through_dirs() {
        let mut state = state_with(tree());
        state.cursor.move_right(&state.roots());
        state.cursor.move_down(&state.roots());
        assert_eq!(
            selected_path(&state),
            Some(work_root().join("src").join("main.rs"))
        );
        state.cursor.clamp(&[]);
        assert_eq!(selected_path(&state), None, "empty tree resolves nothing");
    }

    #[test]
    fn scroll_by_moves_the_offset_and_clamps_at_the_ends() {
        let mut state = state_with(tree());
        state.window = 1;
        state.scroll_by(1);
        assert_eq!(state.scroll, 1);
        state.scroll_by(-10);
        assert_eq!(state.scroll, 0, "clamps at the top");
        state.scroll_by(100);
        assert_eq!(
            state.scroll,
            state.cursor.visible(&state.roots()).len() - 1,
            "clamps at the bottom of the content"
        );
    }

    #[test]
    fn scroll_pulls_the_cursor_back_into_view() {
        let mut state = state_with(tree());
        state.window = 1;
        // The cursor sits on row 0; scrolling far down must drag it along.
        state.scroll_by(3);
        let rows = state.cursor.visible(&state.roots());
        let at = rows
            .iter()
            .position(|row| row.path == state.cursor.selected())
            .expect("cursor on a visible row");
        assert!(at >= state.scroll && at < state.scroll + state.window);
        // And back up: the cursor returns to the top row.
        state.scroll_by(-10);
        assert_eq!(state.cursor.selected(), &[0], "clamped back to the top");
    }

    #[test]
    fn page_moves_one_window_at_a_time() {
        let tall: Vec<TreeNode> = (0..6)
            .map(|n| TreeNode::file(format!("f{n}.txt")))
            .collect();
        let mut state = state_with(tall);
        state.window = 2;
        state.page(1);
        assert_eq!(state.scroll, 2);
        state.page(-1);
        assert_eq!(state.scroll, 0);
        state.page(-5);
        assert_eq!(state.scroll, 0, "no wrap past the top");
    }

    #[test]
    fn pageup_and_pagedown_route_to_the_offset() {
        let tall: Vec<TreeNode> = (0..6)
            .map(|n| TreeNode::file(format!("f{n}.txt")))
            .collect();
        let mut state = state_with(tall);
        state.window = 2;
        assert_eq!(
            handle_key(&mut state, key(KeyCode::PageDown), None),
            Event::Consumed
        );
        assert_eq!(state.scroll, 2);
        assert_eq!(
            handle_key(&mut state, key(KeyCode::PageUp), None),
            Event::Consumed
        );
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn cursor_moves_follow_the_offset() {
        let mut state = state_with(tree());
        state.window = 1;
        state.scroll = 2;
        handle_key(&mut state, key(KeyCode::Up), None);
        let rows = state.cursor.visible(&state.roots());
        let at = rows
            .iter()
            .position(|row| row.path == state.cursor.selected())
            .expect("cursor visible");
        assert!(at < state.scroll + state.window, "the clamp followed");
    }
}
