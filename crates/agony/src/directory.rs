//! The app directory window (Part11 §11.3): the catalog as one
//! internal window — a suggest-engine filter over the merged catalog,
//! grouped installed / available / GPL-shelf when unfiltered, ranked
//! when filtering. Each row names description, license and source.
//! Enter on an available entry asks once — the exact command shows
//! before anything runs (11.3.2's confirm step; installing software is
//! outward-facing) — then the app runs it through the one installer,
//! with one progress line and one result line (11.4.2). GPL entries
//! install through their recipes like any other: the difference is the
//! visible license, never the automation (10.3.3's rule).
//!
//! Items: `State` — filter, cursor, scroll, entries, the pending
//! confirm, the progress/result lines; `DirEntry` — one catalog app
//! plus its install state; `Row` — one display row; `Pending` — the
//! confirmed install waiting for its Enter; `Action` — what the app
//! runs (a package command or the release download); `Event` — what a
//! key or click asked; `Outcome` — the install task's report.
//! Core functions: `entries_from` — the discovery snapshot as
//! directory entries; `rows` / `selectable` — the grouped or ranked
//! view; `handle_key` / `click` / `double_click` — the one cursor
//! machine; `scroll_by` — the wheel; `rect` / `draw` — the window;
//! `group_of` — the installed/available/shelf rule.
//! Depends on: agony_core::manifest (license_is_copyleft, install
//! recipes), agony_core::suggest + layout, crate::discover (the
//! snapshot type), crate::installer (the dispatch), crate::theme.
//! Called by: app (lifecycle, keys, pointer, wheel, draw) and wm
//! (render).
//! Calls: nothing async — running installs is the app's job.

use agony_core::layout::Rect;
use agony_core::manifest::{license_is_copyleft, AppManifest};
use agony_core::suggest::{self, FieldItem};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::discover;
use crate::installer;
use crate::theme::{Applier, Role};

const INSTALLED_HEADER: &str = "installed";
const AVAILABLE_HEADER: &str = "available";
const SHELF_HEADER: &str = "gpl shelf (external download)";

/// One catalog app as the directory shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub manifest: AppManifest,
    pub installed: bool,
}

/// What a confirmed install runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Package(String),
    /// The manifest rides along: the install task needs it for the
    /// archive walk, and the enum stays small enough to move freely.
    Release(Box<(agony_core::manifest::ReleaseSpec, AppManifest)>),
}

impl Action {
    pub fn release(spec: agony_core::manifest::ReleaseSpec, manifest: AppManifest) -> Action {
        Action::Release(Box::new((spec, manifest)))
    }
}

/// The confirm step's payload: what will run, shown before it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub id: String,
    pub action: Action,
}

impl Pending {
    pub fn command_text(&self) -> String {
        match &self.action {
            Action::Package(command) => command.clone(),
            Action::Release(boxed) => format!(
                "download {} (sha256-verified) into <config>\\agony\\bin",
                boxed.0.url
            ),
        }
    }
}

/// The install task's report, one per run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub id: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct State {
    pub filter: String,
    pub selected: usize,
    pub scroll: usize,
    /// The visible row count, updated at draw.
    pub window: usize,
    pub entries: Vec<DirEntry>,
    /// The pending confirm — boxed, like the event that carries it.
    pub pending: Option<Box<Pending>>,
    /// The one progress line (11.4.2), while a task runs.
    pub progress: Option<String>,
    /// The one result line (11.4.2), after it lands.
    pub result: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            filter: String::new(),
            selected: 0,
            scroll: 0,
            window: 10,
            entries: Vec::new(),
            pending: None,
            progress: None,
            result: None,
        }
    }
}

impl State {
    /// Replace the entries, keeping the cursor on the same app when it
    /// survived (the post-install rebuild's shape).
    pub fn set_entries(&mut self, entries: Vec<DirEntry>) {
        let kept = self
            .selectable()
            .get(self.selected)
            .and_then(|row| match row {
                Row::Entry { index } => self.entries.get(*index).map(|e| e.manifest.id.clone()),
                _ => None,
            });
        self.entries = entries;
        if let Some(id) = kept {
            if let Some(position) = self.selectable().iter().position(
                |row| matches!(row, Row::Entry { index } if self.entries[*index].manifest.id == id),
            ) {
                self.selected = position;
            }
        }
        self.clamp_scroll();
    }
}

/// The discovery snapshot as directory entries — everything but the
/// carapace candidates (the directory is the manifests' home).
pub fn entries_from(cache: &discover::CatalogCache) -> Vec<DirEntry> {
    cache
        .apps
        .iter()
        .filter(|app| !app.candidate)
        .map(|app| DirEntry {
            manifest: app.manifest.clone(),
            installed: app.path.is_some(),
        })
        .collect()
}

/// One display row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Header(&'static str),
    Entry { index: usize },
}

/// The grouping rule: installed entries first; among the rest, the GPL
/// family gets its own shelf (the visible license line), then
/// everything available.
fn group_of(entry: &DirEntry) -> &'static str {
    if entry.installed {
        return INSTALLED_HEADER;
    }
    if license_is_copyleft(&entry.manifest.license) {
        return SHELF_HEADER;
    }
    AVAILABLE_HEADER
}

fn ordered_groups() -> [&'static str; 3] {
    [INSTALLED_HEADER, AVAILABLE_HEADER, SHELF_HEADER]
}

impl State {
    /// The display rows: ranked when filtering, grouped otherwise.
    pub fn rows(&self) -> Vec<Row> {
        if self.filter.is_empty() {
            return grouped_rows(&self.entries);
        }
        ranked_rows(&self.filter, &self.entries)
    }

    pub fn selectable(&self) -> Vec<Row> {
        self.rows()
            .into_iter()
            .filter(|row| !matches!(row, Row::Header(_)))
            .collect()
    }

    /// Wheel scroll: move the offset, keep the selection in view.
    pub fn scroll_by(&mut self, rows: i32) {
        self.scroll = shift(self.scroll, rows);
        self.clamp_scroll();
    }

    pub fn clamp_scroll(&mut self) {
        let rows_len = self.rows().len();
        let visible = self.window.max(1);
        self.scroll = self.scroll.min(rows_len.saturating_sub(visible));
        let Some(position) = self.selected_position() else {
            return;
        };
        if position >= self.scroll && position < self.scroll + visible {
            return;
        }
        let positions = self.selectable_positions();
        let target = positions
            .iter()
            .find(|position| **position >= self.scroll)
            .or(positions.last());
        if let Some(&target) = target {
            if let Some(at) = positions.iter().position(|p| *p == target) {
                self.selected = at;
            }
        }
    }

    fn selected_position(&self) -> Option<usize> {
        let selected = self.selectable().get(self.selected).copied()?;
        self.rows().iter().position(|row| *row == selected)
    }

    fn selectable_positions(&self) -> Vec<usize> {
        self.rows()
            .iter()
            .enumerate()
            .filter_map(|(position, row)| self.selectable().contains(row).then_some(position))
            .collect()
    }
}

fn shift(offset: usize, rows: i32) -> usize {
    if rows >= 0 {
        offset.saturating_add(rows as usize)
    } else {
        offset.saturating_sub(rows.unsigned_abs() as usize)
    }
}

fn grouped_rows(entries: &[DirEntry]) -> Vec<Row> {
    let mut rows = Vec::new();
    for group in ordered_groups() {
        let items: Vec<Row> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| group_of(entry) == group)
            .map(|(index, _)| Row::Entry { index })
            .collect();
        if items.is_empty() {
            continue;
        }
        rows.push(Row::Header(group));
        rows.extend(items);
    }
    rows
}

/// The filtered view: the suggest engine over name, description,
/// license and category.
fn ranked_rows(filter: &str, entries: &[DirEntry]) -> Vec<Row> {
    let items: Vec<FieldItem> = entries
        .iter()
        .map(|entry| FieldItem {
            label: entry.manifest.name.clone(),
            hint: hint_of(entry),
            fields: vec![
                entry.manifest.name.clone(),
                entry.manifest.description.clone(),
                entry.manifest.license.clone().unwrap_or_default(),
                entry.manifest.category.as_str().to_string(),
            ],
        })
        .collect();
    suggest::rank_fields(filter, &items)
        .into_iter()
        .map(|s| Row::Entry { index: s.index })
        .collect()
}

/// What a key press asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Consumed,
    Close,
    /// The confirm step accepted — the app runs this. Boxed: the
    /// no-data variants stay cheap to move on every key.
    Install(Box<Pending>),
}

pub fn handle_key(state: &mut State, key: KeyEvent) -> Event {
    let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
    match key.code {
        KeyCode::Esc => {
            if state.pending.take().is_some() {
                Event::Consumed
            } else {
                Event::Close
            }
        }
        KeyCode::Enter if plain => activate_selected(state),
        KeyCode::Backspace if plain => {
            state.filter.pop();
            state.selected = 0;
            state.scroll = 0;
            state.clamp_scroll();
            Event::Consumed
        }
        KeyCode::Up if plain => {
            move_selection(state, -1);
            Event::Consumed
        }
        KeyCode::Down if plain => {
            move_selection(state, 1);
            Event::Consumed
        }
        KeyCode::Char(ch) if plain => {
            state.pending = None;
            state.filter.push(ch);
            state.selected = 0;
            state.scroll = 0;
            state.clamp_scroll();
            Event::Consumed
        }
        _ => Event::Consumed,
    }
}

fn move_selection(state: &mut State, delta: i32) {
    let last = state.selectable().len().saturating_sub(1) as i32;
    state.selected = (state.selected as i32 + delta).clamp(0, last) as usize;
    state.clamp_scroll();
}

fn activate_selected(state: &mut State) -> Event {
    let Some(row) = state.selectable().get(state.selected).copied() else {
        return Event::Consumed;
    };
    let Row::Entry { index } = row else {
        return Event::Consumed;
    };
    let Some(entry) = state.entries.get(index) else {
        return Event::Consumed;
    };
    if entry.installed {
        state.result = Some(format!("{} is already installed", entry.manifest.name));
        return Event::Consumed;
    }
    let Some(chosen) = installer::choose_recipe(&entry.manifest) else {
        state.result = Some(format!(
            "{} has no install recipe — see {}",
            entry.manifest.name,
            entry.manifest.source.as_deref().unwrap_or("its source")
        ));
        return Event::Consumed;
    };
    let action = match chosen {
        installer::Chosen::Package { command, .. } => Action::Package(command),
        installer::Chosen::Release(spec) => Action::release(spec, entry.manifest.clone()),
    };
    let pending = Pending {
        id: entry.manifest.id.clone(),
        action,
    };
    state.result = None;
    let pending = Box::new(pending);
    state.pending = Some(pending.clone());
    Event::Install(pending)
}

/// A click selects; a double-click activates (the menu's shape). Both
/// take the wm chrome's inner rect — the same area render_window gets.
pub fn click(state: &mut State, inner: Rect, point: (u16, u16)) -> Event {
    let Some(row) = hit_row(state, inner, point) else {
        return Event::Consumed;
    };
    if let Some(position) = state
        .selectable()
        .iter()
        .position(|candidate| candidate == &row)
    {
        state.selected = position;
        state.clamp_scroll();
    }
    Event::Consumed
}

pub fn double_click(state: &mut State, inner: Rect, point: (u16, u16)) -> Event {
    let Some(row) = hit_row(state, inner, point) else {
        return Event::Consumed;
    };
    if let Some(position) = state
        .selectable()
        .iter()
        .position(|candidate| candidate == &row)
    {
        state.selected = position;
    }
    activate_selected(state)
}

/// The window: a wide centered document.
pub fn rect(desktop: Rect) -> Rect {
    let width = 78.min(desktop.width);
    let height = 20.min(desktop.height);
    let x = desktop.x + desktop.width.saturating_sub(width) / 2;
    let y = desktop.y + desktop.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

/// The content rows between the filter line and the two status lines.
pub fn rows_area(inner: Rect) -> Rect {
    let height = inner.height.saturating_sub(3);
    Rect::new(inner.x, inner.y + 1, inner.width, height)
}

/// The two bottom lines: progress, then result.
pub fn status_area(inner: Rect) -> (Rect, Rect) {
    let progress = Rect::new(inner.x, inner.y + inner.height - 2, inner.width, 1);
    let result = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    (progress, result)
}

fn filter_area(inner: Rect) -> Rect {
    Rect::new(inner.x, inner.y, inner.width, 1)
}

fn hit_row(state: &State, inner: Rect, point: (u16, u16)) -> Option<Row> {
    let items = rows_area(inner);
    if point.1 < items.y || point.1 >= items.y + items.height {
        return None;
    }
    let visible = state.rows().into_iter().skip(state.scroll);
    for (at, display) in visible.enumerate() {
        if at as u16 == point.1 - items.y {
            return match display {
                Row::Header(_) => None,
                other => Some(other),
            };
        }
    }
    None
}

/// Render into the wm chrome's inner rect (the block is the wm's).
pub fn render_window(frame: &mut Frame<'_>, inner: Rect, state: &mut State, theme: &Applier) {
    frame.render_widget(filter_line(state, theme), filter_area(inner));
    let rows = rows_area(inner);
    state.window = rows.height.max(1) as usize;
    state.clamp_scroll();
    let lines = if state.pending.is_some() {
        confirm_lines(state, theme)
    } else {
        list_lines(state, theme, rows)
    };
    frame.render_widget(Paragraph::new(lines), rows);
    let (progress_area, result_area) = status_area(inner);
    let progress = state.progress.clone().unwrap_or_default();
    frame.render_widget(
        Paragraph::new(progress).style(theme.style(Role::Accent, Role::Surface)),
        progress_area,
    );
    let result_style = match state.result.as_deref() {
        Some(text) if text.starts_with("failed") => theme.style(Role::Danger, Role::Surface),
        _ => theme.style(Role::TextDim, Role::Surface),
    };
    let result = state.result.clone().unwrap_or_default();
    frame.render_widget(Paragraph::new(result).style(result_style), result_area);
}

fn filter_line(state: &State, theme: &Applier) -> Paragraph<'static> {
    let text = format!(" search {}_", state.filter);
    Paragraph::new(text).style(theme.style(Role::TextDim, Role::Surface))
}

fn confirm_lines(state: &State, theme: &Applier) -> Vec<Line<'static>> {
    let Some(pending) = state.pending.as_ref() else {
        return Vec::new();
    };
    vec![
        Line::from(Span::styled(
            format!("  Install {}?", pending.id),
            theme.style(Role::Text, Role::Surface),
        )),
        Line::from(Span::styled(
            format!("  {}", pending.command_text()),
            theme.style(Role::Accent, Role::Surface),
        )),
        Line::from(Span::styled(
            "  Enter runs it · Esc cancels",
            theme.style(Role::TextDim, Role::Surface),
        )),
    ]
}

fn list_lines(state: &State, theme: &Applier, area: Rect) -> Vec<Line<'static>> {
    let visible = area.height.max(1) as usize;
    if state.selectable().is_empty() {
        return vec![
            Line::from("(no matches)".to_string()).style(theme.style(Role::TextDim, Role::Surface))
        ];
    }
    let selected_row = state.selectable().get(state.selected).copied();
    state
        .rows()
        .into_iter()
        .skip(state.scroll)
        .take(visible)
        .filter_map(|row| match row {
            Row::Header(name) => Some(
                Line::from(format!("   {name} ─────"))
                    .style(theme.style(Role::TextDim, Role::Surface)),
            ),
            Row::Entry { index } => {
                let entry = state.entries.get(index)?;
                Some(entry_line(selected_row == Some(row), entry, theme))
            }
        })
        .collect()
}

fn entry_line(selected: bool, entry: &DirEntry, theme: &Applier) -> Line<'static> {
    let name_style = if selected {
        theme.style(Role::Background, Role::Accent)
    } else if entry.installed {
        theme.style(Role::Text, Role::Surface)
    } else {
        theme.style(Role::TextDim, Role::Surface)
    };
    Line::from(vec![
        Span::styled(format!("   {} ", entry.manifest.name), name_style),
        Span::styled(
            format!("— {}", hint_of(entry)),
            theme.style(Role::TextDim, Role::Surface),
        ),
    ])
}

fn hint_of(entry: &DirEntry) -> String {
    let license = entry
        .manifest
        .license
        .as_deref()
        .unwrap_or("license unknown");
    let source = entry.manifest.source.as_deref().unwrap_or("-");
    format!("{} — {license} — {source}", entry.manifest.description)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::buffer_text;
    use agony_core::manifest::{Category, InstallSpec};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn entry(id: &str, license: Option<&str>, installed: bool) -> DirEntry {
        let mut manifest = AppManifest::minimal(id, id, id, Category::Dev);
        manifest.description = format!("{id} description");
        manifest.license = license.map(str::to_string);
        manifest.source = Some(format!("https://example.com/{id}"));
        DirEntry {
            manifest,
            installed,
        }
    }

    fn demo() -> Vec<DirEntry> {
        vec![
            entry("bat", Some("MIT OR Apache-2.0"), true),
            entry("fzf", Some("MIT"), false),
            entry("kitty", Some("GPL-3.0"), false),
            entry("no-license", None, false),
        ]
    }

    #[test]
    fn groups_order_installed_available_then_the_shelf() {
        let state = State {
            entries: demo(),
            ..State::default()
        };
        assert_eq!(
            state.rows(),
            vec![
                Row::Header(INSTALLED_HEADER),
                Row::Entry { index: 0 },
                Row::Header(AVAILABLE_HEADER),
                Row::Entry { index: 1 },
                Row::Entry { index: 3 },
                Row::Header(SHELF_HEADER),
                Row::Entry { index: 2 },
            ]
        );
    }

    #[test]
    fn filtering_ranks_across_the_whole_catalog() {
        let mut state = State {
            entries: demo(),
            filter: "chess".into(),
            ..State::default()
        };
        assert!(
            state
                .rows()
                .iter()
                .all(|row| !matches!(row, Row::Header(_))),
            "filtering flattens the groups"
        );
        state.filter = "fzf".into();
        assert_eq!(
            state.rows().first().copied(),
            Some(Row::Entry { index: 1 }),
            "the exact name ranks first: {:?}",
            state.rows()
        );
    }

    #[test]
    fn enter_on_available_asks_and_confirms() {
        let mut state = State {
            entries: demo(),
            ..State::default()
        };
        state.entries[1].manifest.install = Some(InstallSpec {
            scoop: Some("fzf".into()),
            ..InstallSpec::default()
        });
        state.selected = position_of(&state, "fzf");
        let event = handle_key(&mut state, key(KeyCode::Enter));
        let Event::Install(pending) = event else {
            panic!("the confirm step: {event:?}");
        };
        assert_eq!(pending.id, "fzf");
        assert_eq!(
            pending.command_text(),
            "scoop install fzf",
            "the exact command shows before it runs"
        );
        // Esc cancels the pending confirm; Esc again closes.
        assert_eq!(handle_key(&mut state, key(KeyCode::Esc)), Event::Consumed);
        assert_eq!(handle_key(&mut state, key(KeyCode::Esc)), Event::Close);
    }

    #[test]
    fn enter_on_installed_and_recipeless_says_so() {
        let mut state = State {
            entries: demo(),
            ..State::default()
        };
        state.selected = position_of(&state, "bat");
        assert_eq!(handle_key(&mut state, key(KeyCode::Enter)), Event::Consumed);
        assert_eq!(state.result.as_deref(), Some("bat is already installed"));
        state.selected = position_of(&state, "no-license");
        let _ = handle_key(&mut state, key(KeyCode::Enter));
        assert!(
            state
                .result
                .as_deref()
                .is_some_and(|text| text.contains("no install recipe")),
            "{:?}",
            state.result
        );
    }

    #[test]
    fn gpl_entries_install_like_any_other_with_the_license_visible() {
        let mut state = State {
            entries: demo(),
            ..State::default()
        };
        state.entries[2].manifest.install = Some(InstallSpec {
            scoop: Some("kitty".into()),
            ..InstallSpec::default()
        });
        state.selected = position_of(&state, "kitty");
        let event = handle_key(&mut state, key(KeyCode::Enter));
        assert!(
            matches!(event, Event::Install(ref pending) if pending.id == "kitty"),
            "the GPL shelf installs through its recipe: {event:?}"
        );
    }

    fn position_of(state: &State, id: &str) -> usize {
        state
            .selectable()
            .iter()
            .position(|row| matches!(row, Row::Entry { index } if state.entries[*index].manifest.id == id))
            .expect("entry selectable")
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn set_entries_keeps_the_cursor_on_its_app() {
        let mut state = State::default();
        state.entries = demo();
        state.selected = position_of(&state, "fzf");
        let mut next = demo();
        next[1].installed = true;
        state.set_entries(next);
        let selected = state.selectable()[state.selected];
        assert_eq!(
            selected,
            Row::Entry { index: 1 },
            "the same app stays under the cursor after a rebuild"
        );
    }

    #[test]
    fn scroll_clamps_at_both_ends() {
        let mut state = State {
            entries: demo(),
            window: 2,
            ..State::default()
        };
        state.scroll_by(50);
        assert_eq!(state.scroll, state.rows().len() - 2);
        state.scroll_by(-50);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn rect_and_areas_partition_the_window() {
        let desktop = Rect::new(0, 0, 100, 30);
        let region = rect(desktop);
        assert_eq!(region, Rect::new(11, 5, 78, 20));
        let inner = Rect::new(region.x + 1, region.y + 1, 76, 18);
        let rows = rows_area(inner);
        assert_eq!(rows.height, 15, "filter line and two status lines");
        let (progress, result) = status_area(inner);
        assert_eq!(progress.y, inner.y + inner.height - 2);
        assert_eq!(result.y, inner.y + inner.height - 1);
    }

    #[test]
    fn drawn_directory_shows_groups_licenses_and_confirm() {
        let theme = Applier::fallback();
        let mut state = State {
            entries: demo(),
            ..State::default()
        };
        state.entries[1].manifest.install = Some(InstallSpec {
            scoop: Some("fzf".into()),
            ..InstallSpec::default()
        });
        let inner = Rect::new(1, 1, 76, 18);
        let mut terminal = Terminal::new(TestBackend::new(78, 20)).expect("terminal");
        terminal
            .draw(|f| render_window(f, inner, &mut state, &theme))
            .expect("draw");
        let text = buffer_text(&terminal);
        for expected in ["installed", "available", "gpl shelf", "MIT", "GPL-3.0"] {
            assert!(text.contains(expected), "{expected} missing: {text:?}");
        }
        // The confirm step swaps the list for the question.
        let mut confirming = state.clone();
        confirming.pending = Some(Box::new(Pending {
            id: "fzf".into(),
            action: Action::Package("scoop install fzf".into()),
        }));
        let mut terminal = Terminal::new(TestBackend::new(78, 20)).expect("terminal");
        terminal
            .draw(|f| render_window(f, inner, &mut confirming, &theme))
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("Install fzf?"), "{text:?}");
        assert!(text.contains("scoop install fzf"), "{text:?}");
        assert!(text.contains("Esc cancels"), "{text:?}");
    }

    #[test]
    fn click_selects_and_double_click_activates() {
        let mut state = State {
            entries: demo(),
            ..State::default()
        };
        let inner = Rect::new(1, 1, 76, 18);
        // bat is display row 1 (after the installed header), at screen
        // row 3: inner.y (1) + filter (1) + display row 1.
        let event = click(&mut state, inner, (20, 3));
        assert_eq!(event, Event::Consumed);
        assert_eq!(state.selected, 0);
        let event = double_click(&mut state, inner, (20, 3));
        assert_eq!(event, Event::Consumed, "installed entries do not install");
        assert_eq!(state.result.as_deref(), Some("bat is already installed"));
    }
}
