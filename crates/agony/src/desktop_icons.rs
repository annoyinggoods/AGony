//! The desktop icon grid (Part09 §9.4): `<config>/agony/Desktop/`
//! rendered as column-major tiles — app shortcut stubs (small TOML
//! referencing a manifest id, written by Pin to desktop), documents
//! (real files) and folders (directories; activation re-roots the
//! docked panel). A tile is 5×4: a 5×2 block in its kind color with the
//! entry's first two letters centered, then the name below in dim lines.
//! Click/arrows select, double-click/Enter activate; tiles paint under
//! every window (the WM's z-order, unchanged).
//!
//! The grid refreshes at the panel's cadence — on menu close, on panel
//! focus regain, and after a pin/unpin — never on a timer while the user
//! reads.
//!
//! Items: `State` — the entries plus the core's `GridCursor`;
//! `Tile` — one entry (name, path, kind); `Kind` — shortcut (with the
//! manifest id), folder, or document.
//! Core functions: `scan` — the directory listing (shortcuts, then
//! folders, then documents, alphabetical within each); `State::refresh`
//! — rescan at the stated cadence; `pin` / `unpin` — the stub's write
//! and remove; `tile_at` — the hit test; `move_cursor` — the keyboard
//! path; `grid` — the one `layout::icon_grid` the draw and the hit test
//! share; `draw` — the tiles (kind colors through the applier, never a
//! literal).
//! Depends on: agony_core::shortcut (the stub schema), agony_core::layout
//! (icon_grid, GridCursor, initials), theme.
//! Called by: app (scan cadence, keys, pointer, draw under the windows).
//! Calls: std::fs under the desktop dir only.

use std::path::{Path, PathBuf};

use agony_core::icon::initials;
use agony_core::layout::{icon_grid, GridCursor, IconGrid, Rect};
use agony_core::shortcut::Shortcut;
use ratatui::layout::Alignment;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::settings;
use crate::theme::{Applier, Role};

/// The tile's shape (Part09 9.4.3).
pub const TILE_W: u16 = 5;
pub const TILE_H: u16 = 4;
/// The name's budget below the block: two lines of the tile's width.
const NAME_LINES: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A pinned app: the stub references its catalog manifest id.
    Shortcut {
        id: String,
    },
    /// A directory on the desktop: activation re-roots the docked panel.
    Folder,
    Document,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub entries: Vec<Tile>,
    pub cursor: GridCursor,
}

impl State {
    /// Rescan the AGony desktop (the stated cadence calls this).
    pub fn refresh(&mut self) {
        self.entries = scan(&settings::desktop_dir());
        self.cursor.clamp(self.entries.len());
    }
}

/// The one grid the draw and the hit test share.
pub fn grid(desktop: Rect) -> IconGrid {
    icon_grid(desktop, TILE_W, TILE_H)
}

/// The AGony desktop's listing: shortcuts first, then folders, then
/// documents — alphabetical case-insensitively within each rank. A .toml
/// that is not a shortcut stub is a document like any other file.
pub fn scan(dir: &Path) -> Vec<Tile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut tiles: Vec<Tile> = entries
        .flatten()
        .map(|entry| tile_of(&entry.path()))
        .collect();
    tiles.sort_by(|a, b| {
        kind_rank(a, b).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    tiles
}

/// The listing's rank order: shortcuts, then folders, then documents.
fn kind_rank(a: &Tile, b: &Tile) -> std::cmp::Ordering {
    let rank = |tile: &Tile| match tile.kind {
        Kind::Shortcut { .. } => 0,
        Kind::Folder => 1,
        Kind::Document => 2,
    };
    rank(a).cmp(&rank(b))
}

fn tile_of(path: &Path) -> Tile {
    if path.is_dir() {
        return Tile {
            name: file_label(path),
            path: path.to_path_buf(),
            kind: Kind::Folder,
        };
    }
    let name = file_stem_label(path);
    if path.extension().map(|ext| ext == "toml").unwrap_or(false) {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(stub) = Shortcut::from_toml(&text) {
                return Tile {
                    name,
                    path: path.to_path_buf(),
                    kind: Kind::Shortcut { id: stub.id },
                };
            }
        }
    }
    Tile {
        name,
        path: path.to_path_buf(),
        kind: Kind::Document,
    }
}

/// A file tile's name: the stem, so "notes.txt" reads "notes".
fn file_stem_label(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A folder tile's name: the whole name — a directory keeps its dots.
fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Write a shortcut stub for `id` (Pin to desktop); returns the stub's
/// path. The tile appears at the next refresh — never a restart.
pub fn pin(dir: &Path, id: &str) -> Result<PathBuf, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    let stub = Shortcut { id: id.to_string() };
    let path = dir.join(format!("{id}.toml"));
    std::fs::write(&path, stub.to_toml().unwrap_or_default())?;
    Ok(path)
}

/// Remove a shortcut stub (Unpin). Only stubs unpin — documents wait for
/// a trash (the app refuses with a status line).
pub fn unpin(dir: &Path, id: &str) -> Result<(), std::io::Error> {
    std::fs::remove_file(dir.join(format!("{id}.toml")))
}

/// The tile index under a point on the desktop, or None.
pub fn tile_at(desktop: Rect, entries: &[Tile], point: (u16, u16)) -> Option<usize> {
    let index = grid(desktop).tile_at(point.0, point.1)?;
    entries.get(index).map(|_| index)
}

/// The keyboard path (Part06's parity rule): arrows move the cursor over
/// the entries with the core's wrap-at-edges math.
pub fn move_cursor(state: &mut State, desktop: Rect, columns: i32, rows: i32) {
    state
        .cursor
        .move_by(&grid(desktop), state.entries.len(), columns, rows);
}

// ---------------------------------------------------------------- render

/// The tiles on the bare desktop — under every window (z-order is the
/// WM's; a maximized window simply covers the grid).
pub fn draw(frame: &mut Frame<'_>, desktop: Rect, state: &State, theme: &Applier) {
    let grid = grid(desktop);
    for (index, tile) in state.entries.iter().enumerate() {
        let Some(rect) = grid.rect(index) else {
            break;
        };
        draw_tile(frame, rect, tile, index == state.cursor.selected, theme);
    }
}

fn draw_tile(frame: &mut Frame<'_>, rect: Rect, tile: &Tile, selected: bool, theme: &Applier) {
    let (block_role, label_role) = if selected {
        (Role::Focus, Role::Accent)
    } else {
        kind_roles(tile)
    };
    let block = theme_block(block_role, theme);
    let mut block_rect = rect;
    block_rect.height = block_rect.height.min(2);
    frame.render_widget(block, block_rect);
    let letters = Line::from(letters_text(tile))
        .style(
            theme
                .style(Role::Background, block_role)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);
    frame.render_widget(letters, Rect::new(rect.x, rect.y + 1, rect.width, 1));
    for (at, line) in name_lines(tile).into_iter().enumerate() {
        let style = if selected {
            theme.style(Role::Accent, Role::Background)
        } else {
            theme.style(label_role, Role::Background)
        };
        let name_area = Rect::new(rect.x, rect.y + 2 + at as u16, rect.width, 1);
        frame.render_widget(Line::styled(line, style), name_area);
    }
}

/// The kind's (block, name) role pair — the theme decides, never a
/// literal (9.4.3's rule). Folders take the focus role: distinct from
/// the shortcut's accent and the document's surface, with no new palette
/// slot (a fourth role would break every existing theme pack).
fn kind_roles(tile: &Tile) -> (Role, Role) {
    match tile.kind {
        Kind::Shortcut { .. } => (Role::Accent, Role::Text),
        Kind::Folder => (Role::Focus, Role::Text),
        Kind::Document => (Role::Surface, Role::TextDim),
    }
}

fn theme_block(role: Role, theme: &Applier) -> Paragraph<'static> {
    Paragraph::new("").style(theme.style(role, role))
}

fn letters_text(tile: &Tile) -> String {
    let [first, second] = initials(&tile.name);
    format!("{first}{second}")
}

/// The name under the block: up to two lines of the tile's width, an
/// ellipsis marking a cut.
fn name_lines(tile: &Tile) -> Vec<String> {
    let budget = TILE_W as usize * NAME_LINES;
    let mut text: String = tile.name.chars().take(budget).collect();
    if tile.name.chars().count() > budget {
        text.pop();
        text.push('…');
    }
    let lines: Vec<String> = text
        .chars()
        .collect::<Vec<char>>()
        .chunks(TILE_W as usize)
        .map(|chunk| chunk.iter().collect())
        .collect();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::buffer_text;
    use ratatui::{backend::TestBackend, Terminal};

    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agony-icons-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture dir");
        dir
    }

    #[test]
    fn scan_lists_shortcuts_then_folders_then_documents_alphabetically() {
        let dir = fixture_dir("scan");
        std::fs::write(dir.join("zebra.txt"), "doc").expect("doc");
        std::fs::write(dir.join("apple.md"), "doc").expect("doc");
        std::fs::create_dir_all(dir.join("a folder")).expect("dir");
        pin(&dir, "glow").expect("stub");
        let tiles = scan(&dir);
        assert_eq!(tiles.len(), 4);
        assert_eq!(tiles[0].kind, Kind::Shortcut { id: "glow".into() });
        assert_eq!(tiles[1].kind, Kind::Folder, "folders rank above documents");
        assert_eq!(tiles[1].name, "a folder");
        assert_eq!(tiles[2].name, "apple");
        assert_eq!(tiles[3].name, "zebra");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_foreign_toml_is_a_document_not_a_shortcut() {
        let dir = fixture_dir("foreign");
        std::fs::write(dir.join("config.toml"), "[app]\nid = \"glow\"\n").expect("foreign");
        let tiles = scan(&dir);
        assert_eq!(tiles[0].kind, Kind::Document, "the stub schema decides");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_keeps_its_whole_name_and_a_folder_kind() {
        let dir = fixture_dir("foldernames");
        std::fs::create_dir_all(dir.join("my.notes")).expect("dir");
        let tiles = scan(&dir);
        assert_eq!(tiles[0].kind, Kind::Folder);
        assert_eq!(tiles[0].name, "my.notes", "a folder keeps its dots");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pin_and_unpin_round_trip_the_stub() {
        let dir = fixture_dir("pin");
        let path = pin(&dir, "glow").expect("pin");
        assert!(path.exists());
        let tiles = scan(&dir);
        assert_eq!(tiles[0].kind, Kind::Shortcut { id: "glow".into() });
        unpin(&dir, "glow").expect("unpin");
        assert!(scan(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tile_at_finds_column_major_indices() {
        let desktop = Rect::new(0, 0, 30, 20);
        let entries: Vec<Tile> = (0..8)
            .map(|n| Tile {
                name: format!("t{n}"),
                path: PathBuf::new(),
                kind: Kind::Document,
            })
            .collect();
        assert_eq!(tile_at(desktop, &entries, (1, 1)), Some(0));
        assert_eq!(tile_at(desktop, &entries, (1, 5)), Some(1), "below is next");
        assert_eq!(
            tile_at(desktop, &entries, (6, 1)),
            Some(5),
            "right is a column"
        );
        assert_eq!(tile_at(desktop, &entries, (30, 1)), None, "outside");
        let few: Vec<Tile> = entries.into_iter().take(3).collect();
        assert_eq!(tile_at(desktop, &few, (6, 1)), None, "past the entries");
    }

    #[test]
    fn cursor_moves_follow_the_core_grid() {
        let mut state = State {
            entries: (0..12)
                .map(|n| Tile {
                    name: format!("t{n}"),
                    path: PathBuf::new(),
                    kind: Kind::Document,
                })
                .collect(),
            cursor: GridCursor::new(),
        };
        let desktop = Rect::new(0, 0, 30, 20);
        move_cursor(&mut state, desktop, 0, 1);
        assert_eq!(state.cursor.selected, 1);
        move_cursor(&mut state, desktop, 1, 0);
        assert_eq!(state.cursor.selected, 6, "right is one column");
        state.entries.truncate(2);
        state.cursor.clamp(state.entries.len());
        assert_eq!(state.cursor.selected, 1);
    }

    #[test]
    fn name_lines_split_and_ellipsize() {
        let tile = Tile {
            name: "Superfile".into(),
            path: PathBuf::new(),
            kind: Kind::Document,
        };
        assert_eq!(
            name_lines(&tile),
            vec!["Super".to_string(), "file".to_string()]
        );
        let long = Tile {
            name: "a-very-long-document-name".into(),
            path: PathBuf::new(),
            kind: Kind::Document,
        };
        let lines = name_lines(&long);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'), "{lines:?}");
    }

    #[test]
    fn drawn_tiles_show_initials_and_names_ascii_safely() {
        let dir = fixture_dir("draw");
        pin(&dir, "glow").expect("stub");
        std::fs::write(dir.join("rep.md"), "doc").expect("doc");
        std::fs::create_dir_all(dir.join("stuff")).expect("dir");
        let state = State {
            entries: scan(&dir),
            cursor: GridCursor::new(),
        };
        let theme = Applier::fallback();
        let backend = TestBackend::new(30, 20);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| draw(f, Rect::new(0, 0, 30, 20), &state, &theme))
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("GL"), "the shortcut's initials: {text:?}");
        assert!(text.contains("RE"), "the document's initials: {text:?}");
        assert!(text.contains("ST"), "the folder's initials: {text:?}");
        assert!(text.contains("glow"), "the stub's name: {text:?}");
        assert!(text.contains("rep"), "the document's name: {text:?}");
        assert!(text.contains("stuff"), "the folder's name: {text:?}");
        assert!(
            !text
                .chars()
                .any(|ch| ('\u{e000}'..='\u{f8ff}').contains(&ch)),
            "no Nerd codepoints on the desktop"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
