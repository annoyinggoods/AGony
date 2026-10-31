//! The command bar (Part07): a bottom overlay above the taskbar — one
//! input line, the suggestion list under it, the context cwd at the
//! line's left. The bar suggests, the user decides, the shell executes:
//! nothing auto-runs; Tab/Enter-on-a-row accept a candidate's text;
//! only Enter on the bare line executes, through the app's launcher.
//!
//! Items: `State` — the bar's whole state (line, context, selection,
//! list offset, candidates, the paste menu); `BarEvent` — what a key
//! press asked the app to do; `Row` — one rendered list row (a history
//! divider or a candidate); `Sources` — one refresh's inputs;
//! `HistoryStore` — the shared history behind a lock.
//! Core functions: `State::open` / `open_with` — the two opens (7.2.5's
//! seam); `handle_key` — the pure modal routing; `refresh` — the
//! candidate pass (merge order history > carapace > catalog > files);
//! `rows` — the render/hit-test row list; `accept` — the one insertion;
//! `rect` / `draw` — the overlay; `click_target` — the pointer's row;
//! `resolve_launch` / `shell_argv` — execution's two shapes;
//! `load_history` / `save` — the atomic JSON persistence (temp +
//! rename); `pane_exit_status` — the failure recorder's poll.
//! Depends on: agony_core::cmdline + history + suggest + pathpick,
//! complete (the only carapace spawner), pastemenu, startmenu (the
//! entry type), fsprobe (roots), theme.
//! Called by: app (lifecycle, keys, pointer, draw) and the `agony
//! history` verb.
//! Calls: complete's cache (never spawns carapace itself).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agony_core::cmdline::{self, Line};
use agony_core::config::TaskbarPosition;
use agony_core::history::{Entry, History};
use agony_core::layout::Rect;
use agony_core::pathpick;
use agony_core::suggest::{self, Candidate, Ranked, Scope, Source};
use agony_core::tree::TreeNode;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::text::{Line as RLine, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::complete;
use crate::pastemenu;
use crate::startmenu;
use crate::theme::{Applier, Role};

/// The list's tallest shape (Part07 7.3.4: at most as many rows as the
/// overlay allows; this is the allowance).
const MAX_LIST_ROWS: usize = 9;
/// The context column's budget before truncation.
const CONTEXT_COLS: usize = 34;
/// History flushes after this much quiet (the write is atomic; the
/// worst case loses the last seconds on a hard kill).
pub const SAVE_IDLE: Duration = Duration::from_secs(2);
/// How long the exit watcher waits for a shell command to finish
/// (240 × 500 ms = 2 minutes; past it the status stays unknown).
const EXIT_WATCH_POLLS: usize = 240;

// ------------------------------------------------------------ history io

/// The run history behind one lock — shared by the bar, the exit
/// watcher task and the CLI verb.
#[derive(Clone, Default)]
pub struct HistoryStore(Arc<Mutex<History>>);

impl HistoryStore {
    /// Load `<dir>/history.json`; unreadable or missing starts empty.
    pub fn load(dir: &Path) -> HistoryStore {
        let history = read_history(dir).unwrap_or_default();
        HistoryStore(Arc::new(Mutex::new(history)))
    }

    /// Record one executed line (the store's `record` handles dedup
    /// and the cap).
    pub fn record(&self, entry: Entry) {
        lock(&self.0).record(entry);
    }

    /// Write a failure's status back onto its entry (the exit
    /// watcher's call; only failures carry one).
    pub fn record_failure(&self, line: &str, timestamp_ms: u64, code: i32) {
        if code == 0 {
            return;
        }
        lock(&self.0).set_status(line, timestamp_ms, code);
    }

    /// The history's completing continuations, cloned (usually a
    /// handful) for ranking.
    pub fn completions(&self, prefix: &str) -> Vec<Entry> {
        lock(&self.0)
            .completions(prefix)
            .into_iter()
            .cloned()
            .collect()
    }

    /// A snapshot for persistence and the CLI.
    pub fn snapshot(&self) -> History {
        lock(&self.0).clone()
    }

    /// Replace the whole content (the CLI's `--clear`).
    pub fn replace(&self, history: History) {
        *lock(&self.0) = history;
    }
}

fn lock(history: &Mutex<History>) -> std::sync::MutexGuard<'_, History> {
    match history.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// `<dir>/history.json`.
pub fn history_path(dir: &Path) -> PathBuf {
    dir.join("history.json")
}

fn read_history(dir: &Path) -> Option<History> {
    let text = std::fs::read_to_string(history_path(dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write the history file atomically (temp file + rename, Part07
/// 7.4.1): a hard kill mid-write leaves either the old file or the new
/// one, never a truncated one.
pub fn save(dir: &Path, history: &History) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let target = history_path(dir);
    let temp = target.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_string(history).unwrap_or_default())?;
    std::fs::rename(&temp, &target)
}

// ------------------------------------------------------------- the bar

/// What a key press asked the app to do (launching and context reads
/// are the app's — nothing here awaits).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BarEvent {
    Consumed,
    Close,
    /// Execute the line as typed (app-vs-shell is the app's call).
    Execute,
}

#[derive(Clone, Debug)]
pub struct State {
    pub line: Line,
    /// The working directory the bar runs in — displayed, never
    /// assumed (7.2.2).
    pub context: String,
    /// Set when the pane's cwd should be re-read (at open, on a
    /// context click).
    pub context_dirty: bool,
    pub selected: Option<usize>,
    /// The list's scroll offset — the wheel and the keys move the same
    /// one (7.3.5, Part06's one-offset rule).
    pub offset: usize,
    /// The visible row count, updated at draw.
    pub window: usize,
    pub candidates: Vec<Candidate>,
    /// The open paste menu and the held file it drops (7.5).
    pub paste: Option<(pastemenu::Menu, String)>,
    /// The settings promise (7.3.6): false leaves history only.
    pub enabled: bool,
    /// The last line edit — the debounce's clock anchor.
    pub edit_at: Instant,
}

impl State {
    /// Open an empty bar in `context`.
    pub fn open(context: &str, enabled: bool) -> State {
        State {
            line: Line::default(),
            context: context.to_string(),
            context_dirty: true,
            selected: None,
            offset: 0,
            window: MAX_LIST_ROWS,
            candidates: Vec::new(),
            paste: None,
            enabled,
            edit_at: Instant::now(),
        }
    }

    /// Open with initial text, cursor at the end — the seam Part09's
    /// start-menu SEARCH carries its filter through (7.2.5); the next
    /// refresh runs the suggestion pass once, as if typed.
    pub fn open_with(context: &str, text: &str, enabled: bool) -> State {
        let mut state = State::open(context, enabled);
        state.line = Line::new(text);
        state
    }

    /// Drop the held file into the bar: open the paste menu over the
    /// current line (7.5.3 calls this with a fresh bar).
    pub fn drop_picked(&mut self, picked: &str) {
        self.paste = Some((pastemenu::Menu::new(), picked.to_string()));
    }
}

/// One refresh's inputs (the app resolves them; none of this awaits).
pub struct Sources<'a> {
    pub entries: &'a [startmenu::Entry],
    pub history: &'a HistoryStore,
    /// The panel's snapshot as (root, roots), when the panel is open —
    /// the file-suggestion corpus (7.3.2).
    pub files: Option<(PathBuf, Vec<TreeNode>)>,
    pub complete: &'a complete::Shared,
}

/// Recompute the candidates: rank each source with the one engine,
/// merge in the fixed order, keep the selection and offset valid.
pub fn refresh(state: &mut State, sources: &Sources<'_>) {
    let history_list = history_candidates(state, sources);
    let carapace_list = carapace_candidates(state, sources);
    let catalog_list = catalog_candidates(state, sources);
    let files_list = file_candidates(state, sources);
    state.candidates =
        suggest::merge_ranked([history_list, carapace_list, catalog_list, files_list]);
    let rows_len = rows(state).len();
    state.offset = state
        .offset
        .min(rows_len.saturating_sub(state.window.max(1)));
    keep_selection_visible(state);
}

/// History: whole lines extending the typed prefix (7.3.2), ranked by
/// the engine against the whole line, newest first.
fn history_candidates(state: &State, sources: &Sources<'_>) -> Vec<Ranked> {
    let prefix = state.line.text.clone();
    let entries = sources.history.completions(&prefix);
    let pairs: Vec<(String, String)> = entries
        .iter()
        .map(|entry| {
            let marker = if entry.status.is_some() { "✗ " } else { "" };
            (entry.line.clone(), format!("{marker}{}", entry.cwd))
        })
        .collect();
    suggest::rank(&prefix, &pairs)
        .into_iter()
        .map(|suggestion| {
            let entry = &entries[suggestion.index];
            Ranked {
                score: suggestion.score,
                candidate: Candidate {
                    insert: entry.line.clone(),
                    display: entry.line.clone(),
                    description: entry.cwd.clone(),
                    source: Source::History,
                    scope: Scope::Line,
                    failed: entry.status.is_some(),
                },
            }
        })
        .collect()
}

/// Carapace: the cached candidates for (command, active word) — the
/// driver's answer re-ranked by the engine against the active word.
fn carapace_candidates(state: &State, sources: &Sources<'_>) -> Vec<Ranked> {
    if !state.enabled || !sources.complete.enabled() {
        return Vec::new();
    }
    let parsed = cmdline::parse(&state.line.text, state.line.cursor);
    let values = cmdline::prefix_values(&state.line.text, state.line.cursor);
    let Some(command) = values.first().cloned() else {
        return Vec::new();
    };
    let Some(cached) = sources.complete.cached(&command, &parsed.active_text) else {
        return Vec::new();
    };
    let pairs: Vec<(String, String)> = cached
        .iter()
        .map(|candidate| (candidate.insert.clone(), candidate.description.clone()))
        .collect();
    suggest::rank(&parsed.active_text, &pairs)
        .into_iter()
        .map(|suggestion| {
            let candidate = &cached[suggestion.index];
            Ranked {
                score: suggestion.score,
                candidate: candidate.clone(),
            }
        })
        .collect()
}

/// The catalog, when editing the first word (7.3.2): matching app
/// names with their descriptions; the insertion is the manifest id.
fn catalog_candidates(state: &State, sources: &Sources<'_>) -> Vec<Ranked> {
    if !state.enabled {
        return Vec::new();
    }
    let parsed = cmdline::parse(&state.line.text, state.line.cursor);
    if parsed.active != 0 {
        return Vec::new();
    }
    let pairs: Vec<(String, String)> = sources
        .entries
        .iter()
        .map(|entry| {
            (
                entry.manifest.id.clone(),
                entry.manifest.description.clone(),
            )
        })
        .collect();
    suggest::rank(&parsed.active_text, &pairs)
        .into_iter()
        .map(|suggestion| {
            let entry = &sources.entries[suggestion.index];
            let mut candidate = Candidate::word(entry.manifest.id.clone(), Source::Catalog).shown();
            candidate.display = entry.manifest.name.clone();
            candidate.description = suggestion.hint;
            Ranked {
                score: suggestion.score,
                candidate,
            }
        })
        .collect()
}

/// File paths from the panel's snapshot, when the active word looks
/// like a path (7.3.2); the insertion is the path relative to the
/// displayed context, quoted — `pathpick`'s forms.
fn file_candidates(state: &State, sources: &Sources<'_>) -> Vec<Ranked> {
    if !state.enabled {
        return Vec::new();
    }
    let parsed = cmdline::parse(&state.line.text, state.line.cursor);
    if !looks_like_a_path(&parsed.active_text) {
        return Vec::new();
    }
    let Some((root, roots)) = &sources.files else {
        return Vec::new();
    };
    let corpus: Vec<suggest::PathItem> = agony_core::tree::flatten_paths(roots, "\\")
        .into_iter()
        .map(|path| suggest::PathItem {
            label: pathpick::file_name(path.trim_end_matches(['\\', '/'])).to_string(),
            path,
        })
        .collect();
    suggest::rank_paths(&parsed.active_text, &corpus)
        .into_iter()
        .map(|suggestion| {
            let absolute = root.join(suggestion.hint.trim_end_matches(['\\', '/']));
            let insert = pathpick::quote_if_needed(&pathpick::relative_path(
                &absolute.display().to_string(),
                &state.context,
            ));
            let mut candidate = Candidate::word(insert, Source::Files).shown();
            candidate.description = suggestion.hint;
            Ranked {
                score: suggestion.score,
                candidate,
            }
        })
        .collect()
}

/// The path-heuristic of 7.3.2: contains a separator or a dot.
fn looks_like_a_path(word: &str) -> bool {
    word.contains('/') || word.contains('\\') || word.contains('.')
}

/// The rendered/hit-tested rows: candidates in merge order, with a
/// divider before each run of history rows (7.3.2's "under a divider").
/// The list's visible row count — the paste menu's three rows when it
/// is open (they need the bar's height; found clipped in the Part10
/// demo run), else the suggestion rows.
pub fn visible_rows(state: &State) -> usize {
    if state.paste.is_some() {
        pastemenu::FORM_COUNT
    } else {
        rows(state).len()
    }
}

pub fn rows(state: &State) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut last_was_history = false;
    for (index, candidate) in state.candidates.iter().enumerate() {
        let is_history = candidate.source == Source::History;
        if is_history && !last_was_history {
            rows.push(Row::Divider);
        }
        rows.push(Row::Candidate(index));
        last_was_history = is_history;
    }
    rows
}

/// One rendered list row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Divider,
    Candidate(usize),
}

/// The overlay's rectangle: the desktop's full width, anchored at the
/// taskbar's edge of the desktop — above a bottom taskbar, below a top
/// one (the prompt that opens it sits on the taskbar) — tall enough for
/// the input line and the visible rows.
pub fn rect(desktop: Rect, visible_rows: usize, position: TaskbarPosition) -> Rect {
    let list = (visible_rows.max(1) as u16).min(MAX_LIST_ROWS as u16);
    let height = (list + 1 + 2).min(desktop.height.max(3));
    let y = match position {
        TaskbarPosition::Top => desktop.y,
        TaskbarPosition::Bottom => desktop.y + desktop.height - height,
    };
    Rect::new(desktop.x, y, desktop.width, height)
}

/// Route one key press inside the bar. `picked` is the held file's
/// path — Ctrl-G drops it into the paste menu (7.5.1). The paste menu,
/// when open, is the modal's modal: Up/Down/Enter/Esc only.
pub fn handle_key(state: &mut State, key: KeyEvent, picked: Option<&str>) -> BarEvent {
    if state.paste.is_some() {
        return paste_key(state, key);
    }
    let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => BarEvent::Close,
        KeyCode::Enter if plain => {
            if state.line.text.trim().is_empty() {
                return BarEvent::Consumed;
            }
            if state.selected.is_some() {
                accept_selected(state);
                return BarEvent::Consumed;
            }
            BarEvent::Execute
        }
        KeyCode::Tab if plain => {
            accept_first(state);
            BarEvent::Consumed
        }
        KeyCode::Right if plain && at_end(state) => {
            accept_first(state);
            BarEvent::Consumed
        }
        KeyCode::Right if plain => {
            state.line.right();
            BarEvent::Consumed
        }
        KeyCode::Left if plain => {
            state.line.left();
            BarEvent::Consumed
        }
        KeyCode::Up if plain => {
            move_selection(state, -1);
            BarEvent::Consumed
        }
        KeyCode::Down if plain => {
            move_selection(state, 1);
            BarEvent::Consumed
        }
        KeyCode::PageUp if plain => {
            scroll(state, -(state.window.max(1) as i32));
            BarEvent::Consumed
        }
        KeyCode::PageDown if plain => {
            scroll(state, state.window.max(1) as i32);
            BarEvent::Consumed
        }
        KeyCode::Home if plain => {
            state.line.home();
            BarEvent::Consumed
        }
        KeyCode::End if plain => {
            state.line.end();
            BarEvent::Consumed
        }
        KeyCode::Backspace if plain => {
            state.line.backspace();
            note_edit(state);
            BarEvent::Consumed
        }
        KeyCode::Delete if plain => {
            state.line.delete();
            note_edit(state);
            BarEvent::Consumed
        }
        KeyCode::Char('f') if ctrl && state.enabled => {
            autofill(state);
            BarEvent::Consumed
        }
        KeyCode::Char('g') if ctrl => {
            if let Some(path) = picked {
                state.drop_picked(path);
            }
            BarEvent::Consumed
        }
        KeyCode::Char(ch) if plain => {
            state.line.insert(ch);
            note_edit(state);
            BarEvent::Consumed
        }
        KeyCode::Char(' ') if ctrl => BarEvent::Consumed,
        _ => BarEvent::Consumed,
    }
}

/// The paste menu's keys (7.5): move, insert the exact preview, cancel
/// back to the line — which keeps the bar's focus.
fn paste_key(state: &mut State, key: KeyEvent) -> BarEvent {
    let plain = key.modifiers.is_empty();
    match key.code {
        KeyCode::Esc => {
            state.paste = None;
            BarEvent::Consumed
        }
        KeyCode::Up if plain => {
            if let Some((menu, _)) = state.paste.as_mut() {
                menu.move_selection(-1);
            }
            BarEvent::Consumed
        }
        KeyCode::Down if plain => {
            if let Some((menu, _)) = state.paste.as_mut() {
                menu.move_selection(1);
            }
            BarEvent::Consumed
        }
        KeyCode::Enter if plain => {
            let insertion = state
                .paste
                .as_ref()
                .map(|(menu, picked)| pastemenu::chosen_text(picked, &state.context, menu))
                .unwrap_or_default();
            state.line.insert_str(&insertion);
            state.paste = None;
            note_edit(state);
            BarEvent::Consumed
        }
        _ => BarEvent::Consumed,
    }
}

/// Every line edit re-anchors the debounce clock and restarts the
/// suggestion view (a stale selection would point past its row).
fn note_edit(state: &mut State) {
    state.edit_at = Instant::now();
    state.selected = None;
    state.offset = 0;
}

fn at_end(state: &State) -> bool {
    state.line.cursor == state.line.text.chars().count()
}

/// Accept the selected candidate (Tab / Enter-on-a-row / → at the end
/// fall back to the first) — a word replaces the active word, a
/// history line replaces the whole line. Nothing executes here.
pub fn accept_first(state: &mut State) {
    if state.selected.is_none() && !state.candidates.is_empty() {
        state.selected = Some(0);
    }
    accept_selected(state);
}

pub fn accept_selected(state: &mut State) {
    let Some(index) = state.selected else {
        return;
    };
    let Some(candidate) = state.candidates.get(index) else {
        return;
    };
    let insert = candidate.insert.clone();
    match candidate.scope {
        Scope::Word => {
            let (text, cursor) =
                cmdline::replace_active(&state.line.text, state.line.cursor, &insert);
            state.line = Line { text, cursor };
        }
        Scope::Line => {
            state.line = Line::new(insert);
        }
    }
    state.selected = None;
    state.offset = 0;
    note_edit(state);
}

/// The autofill key (7.3.4): only when exactly one candidate remains,
/// without the list ever opening wider.
fn autofill(state: &mut State) {
    if state.candidates.len() == 1 {
        state.selected = Some(0);
        accept_selected(state);
    }
}

fn move_selection(state: &mut State, delta: i32) {
    let last = state.candidates.len();
    if last == 0 {
        return;
    }
    let current = state.selected.map(|at| at as i32).unwrap_or(-1);
    let moved = (current + delta).clamp(0, last as i32 - 1);
    state.selected = Some(moved as usize);
    keep_selection_visible(state);
}

/// Wheel scroll (Part06's gestures, 7.3.5): the same offset the keys
/// move, clamped, the selection never lost offscreen.
pub fn scroll(state: &mut State, rows: i32) {
    state.offset = if rows >= 0 {
        state.offset.saturating_add(rows as usize)
    } else {
        state.offset.saturating_sub(rows.unsigned_abs() as usize)
    };
    keep_selection_visible(state);
}

/// The one viewport clamp: the offset stays inside the rendered rows
/// and a selected row stays inside the window.
fn keep_selection_visible(state: &mut State) {
    let rendered = rows(state);
    let visible = state.window.max(1);
    state.offset = state.offset.min(rendered.len().saturating_sub(visible));
    let Some(selected) = state.selected else {
        return;
    };
    let Some(at) = rendered
        .iter()
        .position(|row| *row == Row::Candidate(selected))
    else {
        return;
    };
    if at >= state.offset && at < state.offset + visible {
        return;
    }
    // Pull the selection to the nearest candidate inside the window.
    let window: Vec<usize> = rendered
        .iter()
        .skip(state.offset)
        .take(visible)
        .filter_map(|row| match row {
            Row::Candidate(index) => Some(*index),
            Row::Divider => None,
        })
        .collect();
    let nearest = if at < state.offset {
        window.first().copied()
    } else {
        window.last().copied()
    };
    state.selected = nearest.or(state.selected);
}

// -------------------------------------------------------- execution

/// The bar's first-word rule (7.2.4): a manifest app (by id or name,
/// case-insensitive) launches with the remaining words as argv;
/// everything else is the caller's shell line.
pub fn resolve_launch<'a>(
    entries: &'a [startmenu::Entry],
    values: &[String],
) -> Option<(&'a startmenu::Entry, Vec<String>)> {
    let (first, rest) = values.split_first()?;
    let entry = entries.iter().find(|entry| {
        entry.manifest.id.eq_ignore_ascii_case(first)
            || entry.manifest.name.eq_ignore_ascii_case(first)
    })?;
    Some((entry, rest.to_vec()))
}

/// The shell-command shape of a non-app line (7.2.4): the platform
/// shell invoked with the whole line as one argument — the launcher
/// carries it as argv, never as re-split shell text. The OS fork is the
/// seam's (`platform::shell_command_argv`, Part12 12.2.3).
pub fn shell_argv(line: &str) -> Vec<String> {
    crate::platform::shell_command_argv(line)
}

/// Poll a pane until its process exits, and return the exit code —
/// the failure recorder (7.4.2). Reads the pane-scoped `Pane::info()`
/// snapshot: the window-level one leaves process/exit state `Unknown`
/// by SDK construction, so this poll must go through the pane handle.
/// Two minutes, then give up (the status stays unknown, which the
/// model distinguishes).
pub async fn pane_exit_status(pane: &rmux_sdk::Pane, pane_key: u32) -> Option<i32> {
    use rmux_sdk::PaneId;
    let pane_id = PaneId::from(pane_key);
    for _ in 0..EXIT_WATCH_POLLS {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let Ok(info) = pane.info().await else {
            return None;
        };
        if let Some(exit) = info.pane(pane_id).and_then(|pane| pane.exit_state.as_ref()) {
            return exit.code;
        }
    }
    None
}

// ------------------------------------------------------------ drawing

/// The pointer's meaning inside the bar's rect (7.3.5): the context
/// line, or a list row. None outside.
pub fn click_target(region: Rect, state: &State, point: (u16, u16)) -> Option<Click> {
    let block = Block::bordered();
    let inner = block.inner(region);
    let (col, row) = point;
    if col < inner.x || col >= inner.x + inner.width {
        return None;
    }
    if row == inner.y {
        return Some(Click::Context);
    }
    let at = row.checked_sub(inner.y + 1).map(|delta| delta as usize)?;
    match rows(state).get(at) {
        Some(Row::Candidate(index)) => Some(Click::Row(*index)),
        _ => None,
    }
}

/// What a click inside the bar hit (7.3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// The context line: re-read the pane cwd.
    Context,
    /// A suggestion row (the candidate's index).
    Row(usize),
}

pub fn draw(
    frame: &mut Frame<'_>,
    region: Rect,
    state: &mut State,
    theme: &Applier,
    font: agony_core::config::FontMode,
) {
    // The bar owns every cell of its region: Clear first, so the windows
    // under it never bleed through (the UI review's #1 finding).
    frame.render_widget(Clear, region);
    let block = Block::bordered()
        .border_style(theme.style(Role::Border, Role::Surface))
        .style(theme.style(Role::Text, Role::Surface));
    let inner = block.inner(region);
    frame.render_widget(block, region);
    let list = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    state.window = list.height as usize;
    frame.render_widget(
        input_line(state, theme, font),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    if let Some((menu, picked)) = &state.paste {
        pastemenu::draw(frame, list, menu, picked, &state.context, theme);
        return;
    }
    frame.render_widget(list_paragraph(state, theme), list);
}

/// The input row: the context cwd at the left, the prompt glyph, the
/// line with the cursor cell reversed.
fn input_line(
    state: &State,
    theme: &Applier,
    font: agony_core::config::FontMode,
) -> Paragraph<'static> {
    let context = truncate_chars(&state.context, CONTEXT_COLS);
    let prompt = agony_core::icon::glyph(agony_core::icon::Icon::ChevronRight, font);
    let dim = theme.style(Role::TextDim, Role::Surface);
    let text = theme.style(Role::Text, Role::Surface);
    let chars: Vec<char> = state.line.text.chars().collect();
    let before: String = chars[..state.line.cursor.min(chars.len())].iter().collect();
    let under = chars.get(state.line.cursor).copied();
    let after: String = chars[(state.line.cursor + 1).min(chars.len())..]
        .iter()
        .collect();
    let cursor_style = text.add_modifier(Modifier::REVERSED);
    let mut spans = vec![
        Span::styled(format!(" {context} "), dim),
        Span::styled(format!("{prompt} "), dim),
        Span::styled(before, text),
    ];
    match under {
        Some(ch) => {
            spans.push(Span::styled(ch.to_string(), cursor_style));
        }
        None => spans.push(Span::styled(" ", cursor_style)),
    }
    spans.push(Span::styled(after, text));
    Paragraph::new(RLine::from(spans))
}

/// The suggestion list: labels left, descriptions right-aligned and
/// dim, the history divider between sources, `✗` on recorded failures.
fn list_paragraph(state: &State, theme: &Applier) -> Paragraph<'static> {
    let rendered = rows(state);
    let visible = state.window.max(1);
    let lines: Vec<RLine<'static>> = rendered
        .iter()
        .skip(state.offset)
        .take(visible)
        .map(|row| match row {
            Row::Divider => {
                RLine::from(" history ─────").style(theme.style(Role::TextDim, Role::Surface))
            }
            Row::Candidate(index) => candidate_line(state, *index, theme),
        })
        .collect();
    Paragraph::new(lines).style(theme.style(Role::Text, Role::Surface))
}

fn candidate_line(state: &State, index: usize, theme: &Applier) -> RLine<'static> {
    let Some(candidate) = state.candidates.get(index) else {
        return RLine::from("");
    };
    let selected = state.selected == Some(index);
    let suggested = state.selected.is_none() && index == 0;
    let style = if selected {
        theme.style(Role::Background, Role::Selection)
    } else if suggested {
        // The visible "best match" affordance (the UI review's
        // invisible-selection finding): accent foreground, never a
        // silent default. Enter still executes the typed line — the
        // suggestion is an offer, not a capture.
        theme.style(Role::Accent, Role::Surface)
    } else if candidate.failed {
        theme.style(Role::TextDim, Role::Surface)
    } else {
        theme.style(Role::Text, Role::Surface)
    };
    let dim = theme.style(Role::TextDim, Role::Surface);
    let marker = if candidate.failed {
        "✗ "
    } else if suggested {
        "▸ "
    } else {
        ""
    };
    let width = state.window.min(200);
    let left = format!(" {marker}{}", candidate.display);
    let left_cols = left.chars().count();
    let desc_cols = candidate.description.chars().count();
    let pad = (width.saturating_sub(1).saturating_sub(left_cols)).saturating_sub(desc_cols);
    RLine::from(vec![
        Span::styled(left, style),
        Span::raw(" ".repeat(pad as usize + 1)),
        Span::styled(candidate.description.clone(), dim),
    ])
}

fn truncate_chars(text: &str, budget: usize) -> String {
    agony_core::textutil::ellipsize_left(text, budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::Shared;
    use agony_core::history::Entry;
    use agony_core::manifest::{AppManifest, Category};

    fn entry(id: &str, name: &str) -> startmenu::Entry {
        startmenu::Entry {
            manifest: AppManifest::minimal(id, name, id, Category::Dev),
            path: Some(format!("C:\\bin\\{id}.exe")),
            candidate: false,
        }
    }

    fn history_entry(line: &str, cwd: &str, at: u64, status: Option<i32>) -> Entry {
        Entry {
            line: line.into(),
            cwd: cwd.into(),
            timestamp_ms: at,
            status,
        }
    }

    fn sources<'a>(
        entries: &'a [startmenu::Entry],
        history: &'a HistoryStore,
        complete: &'a Shared,
    ) -> Sources<'a> {
        Sources {
            entries,
            history,
            files: None,
            complete,
        }
    }

    #[test]
    fn open_with_carries_text_and_runs_as_if_typed() {
        let state = State::open_with(r"C:\repo", "git che", true);
        assert_eq!(state.line.text, "git che");
        assert_eq!(state.line.cursor, 7);
        assert!(state.context_dirty, "the context re-reads at open");
    }

    #[test]
    fn history_completions_rank_as_line_scoped_candidates() {
        let history = HistoryStore::default();
        history.record(history_entry("git checkout main", r"C:\repo", 1, None));
        history.record(history_entry("git status", r"C:\repo", 2, Some(128)));
        let (complete, _) = Shared::start(false);
        let entries = [];
        let mut state = State::open_with(r"C:\repo", "git ", true);
        refresh(&mut state, &sources(&entries, &history, &complete));
        assert!(
            state
                .candidates
                .iter()
                .any(|candidate| candidate.insert == "git checkout main"
                    && candidate.scope == Scope::Line),
            "the run line reappears as a completing continuation"
        );
        let failed = state
            .candidates
            .iter()
            .find(|candidate| candidate.insert == "git status")
            .expect("the failure is remembered");
        assert!(failed.failed, "the ✗ marker rides on the candidate");
    }

    #[test]
    fn catalog_candidates_only_for_the_first_word() {
        let history = HistoryStore::default();
        let (complete, _) = Shared::start(false);
        let entries = [entry("glow", "Glow"), entry("git", "Git")];
        let mut state = State::open_with(r"C:\repo", "gl", true);
        refresh(&mut state, &sources(&entries, &history, &complete));
        assert!(
            state
                .candidates
                .iter()
                .any(|candidate| candidate.insert == "glow"),
            "typing a prefix of an app name offers it"
        );
        let mut second = State::open_with(r"C:\repo", "git gl", true);
        refresh(&mut second, &sources(&entries, &history, &complete));
        assert!(
            !second
                .candidates
                .iter()
                .any(|candidate| candidate.insert == "glow"),
            "the catalog only feeds the first word"
        );
    }

    #[test]
    fn disabled_suggestions_leave_history_only() {
        let history = HistoryStore::default();
        history.record(history_entry("cargo test", r"C:\repo", 1, None));
        let (complete, _) = Shared::start(false);
        let entries = [entry("cargo", "Cargo")];
        let mut state = State::open_with(r"C:\repo", "carg", false);
        refresh(&mut state, &sources(&entries, &history, &complete));
        assert!(
            !state
                .candidates
                .iter()
                .any(|candidate| candidate.source == Source::Catalog),
            "no catalog when suggestions are off"
        );
        assert!(
            !state
                .candidates
                .iter()
                .any(|candidate| candidate.source == Source::Carapace),
            "no carapace when suggestions are off"
        );
    }

    #[test]
    fn enter_accepts_then_executes() {
        let mut state = State::open_with(r"C:\repo", "git che", true);
        state.candidates = vec![Candidate::word("checkout ", Source::Carapace).shown()];
        let key = |code| KeyEvent::new(code, KeyModifiers::empty());
        // Down selects the row, Enter accepts it, a second Enter executes.
        let _ = handle_key(&mut state, key(KeyCode::Down), None);
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter), None),
            BarEvent::Consumed,
            "the first Enter accepts the selected row"
        );
        assert_eq!(state.line.text, "git checkout ");
        assert_eq!(state.selected, None, "the selection cleared");
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter), None),
            BarEvent::Execute,
            "the second Enter executes"
        );
    }

    #[test]
    fn nothing_executes_without_enter() {
        let mut state = State::open_with(r"C:\repo", "git che", true);
        state.candidates = vec![Candidate::word("checkout", Source::Carapace).shown()];
        let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::empty());
        assert_eq!(handle_key(&mut state, key, None), BarEvent::Consumed);
        assert_eq!(state.line.text, "git checkout", "tab only completes");
    }

    #[test]
    fn autofill_takes_only_the_unique_candidate() {
        let mut state = State::open_with(r"C:\repo", "git che", true);
        state.candidates = vec![Candidate::word("checkout", Source::Carapace).shown()];
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        let _ = handle_key(&mut state, ctrl_f, None);
        assert_eq!(state.line.text, "git checkout");
        let mut many = State::open(r"C:\repo", true);
        many.candidates = vec![
            Candidate::word("checkout", Source::Carapace).shown(),
            Candidate::word("check-attr", Source::Carapace).shown(),
        ];
        let _ = handle_key(&mut many, ctrl_f, None);
        assert_eq!(many.line.text, "", "not unique: no autofill");
    }

    #[test]
    fn tab_falls_back_to_the_first_candidate() {
        let mut state = State::open_with(r"C:\repo", "git che", true);
        state.candidates = vec![Candidate::word("checkout", Source::Carapace).shown()];
        let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::empty());
        let _ = handle_key(&mut state, key, None);
        assert_eq!(state.line.text, "git checkout");
    }

    #[test]
    fn wheel_and_pageup_move_the_same_offset() {
        let mut state = State::open(r"C:\repo", true);
        state.candidates = (0..30)
            .map(|n| Candidate::word(format!("c{n}"), Source::Carapace).shown())
            .collect();
        state.window = 4;
        scroll(&mut state, 3);
        assert_eq!(state.offset, 3);
        let page_down = KeyEvent::new(KeyCode::PageDown, KeyModifiers::empty());
        let _ = handle_key(&mut state, page_down, None);
        assert_eq!(state.offset, 7, "keys and wheel share one offset");
        scroll(&mut state, -100);
        assert_eq!(state.offset, 0, "clamped at the top");
    }

    #[test]
    fn scrolling_never_loses_the_selection_offscreen() {
        let mut state = State::open(r"C:\repo", true);
        state.candidates = (0..30)
            .map(|n| Candidate::word(format!("c{n}"), Source::Carapace).shown())
            .collect();
        state.window = 4;
        state.selected = Some(0);
        scroll(&mut state, 20);
        let rendered = rows(&state);
        let at = rendered
            .iter()
            .position(|row| *row == Row::Candidate(state.selected.expect("kept")))
            .expect("selection among rows");
        assert!(
            at >= state.offset && at < state.offset + state.window,
            "selection {at} inside [{}, {})",
            state.offset,
            state.offset + state.window
        );
    }

    #[test]
    fn rows_put_a_divider_before_history_runs() {
        let mut state = State::open(r"C:\repo", true);
        state.candidates = vec![
            Candidate::word("checkout", Source::Carapace).shown(),
            Candidate::word("git status", Source::History).shown(),
            {
                let mut c = Candidate::word("git log", Source::History).shown();
                c.scope = Scope::Line;
                c
            },
            Candidate::word("glow", Source::Catalog).shown(),
        ];
        assert_eq!(
            rows(&state),
            vec![
                Row::Candidate(0),
                Row::Divider,
                Row::Candidate(1),
                Row::Candidate(2),
                Row::Candidate(3),
            ],
            "one divider opens the history run"
        );
    }

    #[test]
    fn click_targets_the_context_line_and_rows() {
        let mut state = State::open(r"C:\repo", true);
        state.candidates = vec![
            Candidate::word("checkout", Source::Carapace).shown(),
            Candidate::word("git status", Source::History).shown(),
            Candidate::word("glow", Source::Catalog).shown(),
        ];
        // region: border, input row, then the list rows, then border.
        let region = rect(Rect::new(0, 0, 80, 20), 4, TaskbarPosition::Bottom);
        let input_row = region.y + 1;
        assert_eq!(click_target(region, &state, (40, 10)), None, "outside");
        assert_eq!(
            click_target(region, &state, (3, input_row)),
            Some(Click::Context),
            "the input row is the context line"
        );
        // List rows top-down: candidate 0, the history divider (no
        // target), candidate 1.
        assert_eq!(click_target(region, &state, (3, input_row + 2)), None);
        assert_eq!(
            click_target(region, &state, (3, input_row + 3)),
            Some(Click::Row(1))
        );
    }

    #[test]
    fn resolve_launch_matches_id_or_name_case_insensitively() {
        let entries = [entry("glow", "Glow")];
        let (match_by_id, rest) =
            resolve_launch(&entries, &["GLOW".into(), "readme.md".into()]).expect("id matches");
        assert_eq!(match_by_id.manifest.id, "glow");
        assert_eq!(rest, ["readme.md"]);
        let (match_by_name, _) = resolve_launch(&entries, &["glow".into()]).expect("name matches");
        assert_eq!(match_by_name.manifest.name, "Glow");
        assert!(resolve_launch(&entries, &["unknown".into()]).is_none());
        assert!(resolve_launch(&entries, &[]).is_none());
    }

    #[test]
    fn shell_argv_carries_the_whole_line_as_one_argument() {
        let argv = shell_argv("git checkout -b main");
        if crate::platform::is_windows() {
            assert_eq!(argv.len(), 4);
            assert_eq!(argv[2], "-Command");
            assert_eq!(argv[3], "git checkout -b main");
        } else {
            assert_eq!(argv, ["sh", "-c", "git checkout -b main"]);
        }
    }

    #[test]
    fn history_save_is_atomic_and_round_trips() {
        let dir = std::env::temp_dir().join("agony-cmdbar-test");
        let _ = std::fs::remove_dir_all(&dir);
        let store = HistoryStore::load(&dir);
        store.record(history_entry("git status", r"C:\repo", 1, None));
        store.record_failure("git status", 1, 128);
        let snapshot = store.snapshot();
        save(&dir, &snapshot).expect("save");
        let reloaded = HistoryStore::load(&dir);
        assert_eq!(reloaded.snapshot(), snapshot, "the file round-trips");
        assert!(
            !dir.join("history.json.tmp").exists(),
            "no temp file survives the rename"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_failure_ignores_successes() {
        let store = HistoryStore::default();
        store.record(history_entry("ok", "cwd", 1, None));
        store.record_failure("ok", 1, 0);
        assert_eq!(store.snapshot().entries[0].status, None);
        store.record_failure("absent", 9, 3);
        assert_eq!(store.snapshot().entries.len(), 1);
    }

    #[test]
    fn paste_menu_moves_and_inserts_the_exact_preview() {
        let mut state = State::open_with(r"C:\work", "type ", true);
        state.drop_picked(r"C:\docs\my report.md");
        let key = |code| KeyEvent::new(code, KeyModifiers::empty());
        let _ = handle_key(&mut state, key(KeyCode::Down), None);
        let _ = handle_key(&mut state, key(KeyCode::Enter), None);
        assert_eq!(state.line.text, r#"type "C:\docs\my report.md""#);
        assert!(state.paste.is_none(), "inserting closes the menu");
        assert_eq!(state.line.cursor, state.line.text.chars().count());
    }

    #[test]
    fn escape_cancels_the_paste_then_the_bar() {
        let mut state = State::open(r"C:\work", true);
        state.drop_picked("report.md");
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(handle_key(&mut state, esc, None), BarEvent::Consumed);
        assert!(state.paste.is_none(), "the first Esc cancels the menu");
        assert_eq!(handle_key(&mut state, esc, None), BarEvent::Close);
    }

    #[test]
    fn bar_rect_anchors_above_the_taskbar_edge() {
        let desktop = Rect::new(0, 0, 80, 23);
        let region = rect(desktop, 4, TaskbarPosition::Bottom);
        assert_eq!(region.width, 80);
        assert_eq!(region.bottom(), 23, "sits on the desktop's bottom edge");
        assert_eq!(region.height, 4 + 1 + 2, "input + list + borders");
        let tall = rect(desktop, 50, TaskbarPosition::Bottom);
        assert_eq!(tall.height, MAX_LIST_ROWS as u16 + 3, "the list caps");
    }

    #[test]
    fn a_top_taskbar_anchors_the_bar_at_the_desktops_top() {
        let desktop = Rect::new(0, 1, 80, 23);
        let region = rect(desktop, 4, TaskbarPosition::Top);
        assert_eq!(region.y, 1, "the bar opens under the top taskbar");
        assert_eq!(region.bottom(), 8, "growth runs downward");
    }

    /// The failure recorder's poll (7.4.2), against the real daemon: a
    /// pane whose child exits with code 3 reports `Some(3)` through the
    /// pane-scoped snapshot — the window-level poll this replaced could
    /// never see an exit (skipped loudly without a daemon).
    #[tokio::test]
    async fn pane_exit_status_reports_the_exited_childs_code() {
        let rmux = match crate::rmuxhost::connect().await {
            Ok(rmux) => rmux,
            Err(error) => {
                eprintln!(
                    "SKIP pane_exit_status_reports_the_exited_childs_code: no daemon ({error})"
                );
                return;
            }
        };
        let argv = crate::platform::exiting_child_argv();
        crate::rmuxhost::ensure_test_anchor(&rmux)
            .await
            .expect("the test anchor exists");
        let (session, pane) = crate::rmuxhost::fresh_named_session(
            &rmux,
            crate::rmuxhost::TEST_SESSION_EXIT,
            80,
            24,
            &argv,
            None,
        )
        .await
        .expect("the exiting child spawns");
        let key = pane
            .id()
            .await
            .ok()
            .flatten()
            .map(|id| id.as_u32())
            .expect("the pane id resolves");

        let status = pane_exit_status(&pane, key).await;

        assert_eq!(status, Some(3), "the child's exit code round-trips");
        let _ = session.kill().await;
    }

    #[test]
    fn context_line_truncates_from_the_left() {
        let shown = truncate_chars(r"C:\very\long\path\that\exceeds\the\budget", 10);
        assert!(shown.starts_with('…'));
        assert!(shown.chars().count() <= 11);
        assert_eq!(truncate_chars("short", 34), "short");
    }
}
