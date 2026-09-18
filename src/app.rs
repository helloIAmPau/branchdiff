//! Application state and the event loop.

use crate::diff::{self, FileDiff};
use crate::editor::Editor;
use crate::git::{self, ChangedFile};
use crate::highlight::Highlighter;
use crate::tree::{self, Node, VisibleRow};
use crate::ui;
use anyhow::Result;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use notify::{RecursiveMode, Watcher};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use ratatui::DefaultTerminal;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Quiet period after the last change before the view is refreshed.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(300);
/// How long input polling blocks before the loop services the debounce timer.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Focus {
    Tree,
    Diff,
}

pub struct App {
    /// The branch (or ref) the working tree is being diffed against.
    pub branch: String,
    /// The branch currently checked out in the working tree (the "head" side).
    pub current_branch: String,
    pub files: Vec<ChangedFile>,
    pub roots: Vec<Node>,
    pub expanded: HashSet<String>,
    pub visible: Vec<VisibleRow>,
    pub selected: usize,
    pub tree_state: ListState,
    pub focus: Focus,
    pub side_by_side: bool,
    /// Whether the left-hand file tree is shown.
    pub show_tree: bool,
    pub diff_cache: HashMap<usize, FileDiff>,
    pub current_file: Option<usize>,
    pub diff_scroll: u16,
    pub diff_hscroll: usize,
    /// Rendered line count of the current diff (set during draw, used to clamp scroll).
    pub diff_content_len: usize,
    /// Inner height of the diff viewport (set during draw, used for paging).
    pub diff_viewport: u16,
    // Panel rectangles, recorded during draw so mouse events can hit-test them.
    pub tree_area: Rect,
    pub tree_inner: Rect,
    pub diff_area: Rect,
    /// When present, the right panel is a modal editor for the current file.
    pub editor: Option<Editor>,
    /// Shared syntax highlighter (syntect syntaxes + theme).
    pub highlighter: Highlighter,
    pub should_quit: bool,
}

fn in_rect(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Whether a filesystem event should trigger a refresh (ignore access-only and
/// bookkeeping events).
fn is_relevant(event: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    )
}

impl App {
    pub fn new(branch: String, files: Vec<ChangedFile>) -> Self {
        let roots = tree::build_tree(&files);
        let mut expanded = HashSet::new();
        tree::all_dir_paths(&roots, &mut expanded);
        let current_branch = git::current_branch().unwrap_or_else(|_| "working tree".to_string());
        let mut app = App {
            branch,
            current_branch,
            files,
            roots,
            expanded,
            visible: Vec::new(),
            selected: 0,
            tree_state: ListState::default(),
            focus: Focus::Tree,
            side_by_side: false,
            show_tree: true,
            diff_cache: HashMap::new(),
            current_file: None,
            diff_scroll: 0,
            diff_hscroll: 0,
            diff_content_len: 0,
            diff_viewport: 20,
            tree_area: Rect::default(),
            tree_inner: Rect::default(),
            diff_area: Rect::default(),
            editor: None,
            highlighter: Highlighter::new(),
            should_quit: false,
        };
        app.recompute_visible();
        app.select_first_file();
        app
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Watch the repository for changes so the diff auto-refreshes. If the
        // watcher can't be set up we simply run without auto-refresh.
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .ok();
        if let Some(w) = watcher.as_mut() {
            let _ = w.watch(Path::new("."), RecursiveMode::Recursive);
        }

        let mut pending_since: Option<Instant> = None;
        let mut need_redraw = true;

        while !self.should_quit {
            if need_redraw {
                terminal.draw(|f| ui::draw(f, self))?;
                need_redraw = false;
            }

            // Coalesce filesystem events: (re)start the debounce timer on any
            // relevant change.
            while let Ok(res) = rx.try_recv() {
                if matches!(res, Ok(ref e) if is_relevant(e)) {
                    pending_since = Some(Instant::now());
                }
            }

            // Apply the refresh once things have been quiet for the debounce
            // window (but never yank state out from under an open editor).
            if let Some(t) = pending_since {
                if t.elapsed() >= REFRESH_DEBOUNCE && self.editor.is_none() {
                    self.refresh();
                    pending_since = None;
                    need_redraw = true;
                }
            }

            // Poll input with a timeout so the debounce timer keeps ticking.
            if event::poll(POLL_INTERVAL)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.handle_key(key);
                        need_redraw = true;
                    }
                    Event::Mouse(m) => {
                        self.handle_mouse(m);
                        need_redraw = true;
                    }
                    Event::Resize(_, _) => need_redraw = true,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Re-read the changed-file list and diffs from git, preserving the current
    /// expansion/selection where possible.
    fn refresh(&mut self) {
        let files = match git::changed_files(&self.branch) {
            Ok(f) => f,
            // Transient git state mid-write — skip this round, try again later.
            Err(_) => return,
        };

        // Remember the file currently shown in the right panel and its scroll so
        // we can restore the viewport if the same file survives the refresh.
        let prev_file = self.current_file.map(|i| self.files[i].path.clone());
        let prev_scroll = self.diff_scroll;
        let prev_hscroll = self.diff_hscroll;

        // The checked-out branch may have changed since the last refresh.
        if let Ok(branch) = git::current_branch() {
            self.current_branch = branch;
        }

        // Preserve the tree's overall state across the reload: if every folder
        // was expanded, expand any folders that appear in the new tree too;
        // otherwise leave the expansion set alone so folders stay compacted.
        let was_fully_expanded = self.tree_fully_expanded();
        self.files = files;
        self.roots = tree::build_tree(&self.files);
        if was_fully_expanded {
            let mut all = HashSet::new();
            tree::all_dir_paths(&self.roots, &mut all);
            self.expanded = all;
        }
        self.diff_cache.clear();
        self.current_file = None;
        // recompute_visible restores the selection by path from the old view.
        self.recompute_visible();
        self.snap_to_file();

        // If the same file is still shown, keep the right panel where it was.
        // load_selected_file (via snap_to_file) reset the scroll to the top; the
        // draw pass clamps diff_scroll to the new content length.
        if let Some(idx) = self.current_file {
            if prev_file.as_deref() == Some(self.files[idx].path.as_str()) {
                self.diff_scroll = prev_scroll;
                self.diff_hscroll = prev_hscroll;
            }
        }
    }

    /// True when every directory in the current tree is expanded (the default
    /// "expand-all" state). An empty tree counts as expanded. Used on refresh to
    /// decide whether newly-appearing folders should start expanded.
    fn tree_fully_expanded(&self) -> bool {
        let mut dirs = HashSet::new();
        tree::all_dir_paths(&self.roots, &mut dirs);
        dirs.iter().all(|d| self.expanded.contains(d))
    }

    fn recompute_visible(&mut self) {
        let prev_path = self.visible.get(self.selected).map(|r| r.path.clone());
        let mut v = Vec::new();
        tree::flatten(&self.roots, &self.expanded, 0, &mut v);
        self.visible = v;
        if let Some(p) = prev_path {
            if let Some(i) = self.visible.iter().position(|r| r.path == p) {
                self.selected = i;
            }
        }
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
        self.tree_state.select(Some(self.selected));
    }

    fn select_first_file(&mut self) {
        if let Some(i) = self.visible.iter().position(|r| r.file_index.is_some()) {
            self.selected = i;
            self.tree_state.select(Some(i));
            self.load_selected_file();
        }
    }

    /// Parse (and cache) the diff for whatever file is currently selected.
    fn load_selected_file(&mut self) {
        let Some(row) = self.visible.get(self.selected) else {
            return;
        };
        let Some(idx) = row.file_index else {
            return;
        };
        if self.current_file == Some(idx) {
            return;
        }
        self.current_file = Some(idx);
        self.diff_scroll = 0;
        self.diff_hscroll = 0;
        if !self.diff_cache.contains_key(&idx) {
            let parsed = match git::file_diff(&self.branch, &self.files[idx]) {
                Ok(raw) => diff::parse_diff(&raw),
                Err(_) => FileDiff {
                    hunks: Vec::new(),
                    binary: false,
                },
            };
            self.diff_cache.insert(idx, parsed);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // While the editor is open it owns every keystroke (vim-style); the only
        // way out is `:q`, which sets `quit`.
        if let Some(ed) = self.editor.as_mut() {
            ed.handle_key(key);
            if ed.quit {
                self.editor = None;
                // Drop the cached diff so it is re-read on next view.
                if let Some(idx) = self.current_file {
                    self.diff_cache.remove(&idx);
                    self.current_file = None;
                    self.load_selected_file();
                }
            }
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('e') => self.open_editor(),
            KeyCode::Char('c') if ctrl => self.should_quit = true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Tree => Focus::Diff,
                    Focus::Diff => Focus::Tree,
                };
            }
            KeyCode::Char('s') => self.side_by_side = !self.side_by_side,
            KeyCode::Char('t') => self.toggle_tree(),
            KeyCode::Char('C') => self.toggle_collapse_all(),
            _ => match self.focus {
                Focus::Tree => self.handle_tree_key(key),
                Focus::Diff => self.handle_diff_key(key, ctrl),
            },
        }
    }

    fn handle_tree_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(),
            KeyCode::Right | KeyCode::Char('l') => self.expand_or_focus(),
            KeyCode::Home | KeyCode::Char('g') => self.goto_first_file(),
            KeyCode::End | KeyCode::Char('G') => self.goto_last_file(),
            _ => {}
        }
    }

    fn handle_diff_key(&mut self, key: KeyEvent, ctrl: bool) {
        let page = self.diff_viewport.saturating_sub(2).max(1);
        let max_scroll = self.diff_content_len.saturating_sub(1) as u16;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.diff_scroll = (self.diff_scroll + 1).min(max_scroll)
            }
            KeyCode::Up | KeyCode::Char('k') => self.diff_scroll = self.diff_scroll.saturating_sub(1),
            KeyCode::PageDown => self.diff_scroll = (self.diff_scroll + page).min(max_scroll),
            KeyCode::PageUp => self.diff_scroll = self.diff_scroll.saturating_sub(page),
            KeyCode::Char('d') if ctrl => {
                self.diff_scroll = (self.diff_scroll + page / 2).min(max_scroll)
            }
            KeyCode::Char('u') if ctrl => self.diff_scroll = self.diff_scroll.saturating_sub(page / 2),
            KeyCode::Left | KeyCode::Char('h') => {
                self.diff_hscroll = self.diff_hscroll.saturating_sub(6)
            }
            KeyCode::Right | KeyCode::Char('l') => self.diff_hscroll += 6,
            KeyCode::Home | KeyCode::Char('g') => {
                self.diff_scroll = 0;
                self.diff_hscroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') => self.diff_scroll = max_scroll,
            KeyCode::Esc => self.focus = Focus::Tree,
            _ => {}
        }
    }

    /// Open the current file in the modal editor. Prefers the working-tree copy,
    /// falling back to the head revision's contents.
    fn open_editor(&mut self) {
        let Some(idx) = self.current_file else {
            return;
        };
        let path = self.files[idx].path.clone();
        let content = std::fs::read_to_string(&path)
            .ok()
            .or_else(|| git::read_file_at(&self.branch, &path).ok())
            .unwrap_or_default();
        let mut ed = Editor::new(path, &content);
        ed.status = "-- editing working tree — :w save · :q quit --".into();
        self.editor = Some(ed);
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        if self.editor.is_some() {
            return;
        }
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if in_rect(self.tree_area, x, y) {
                    self.tree_click(y);
                } else if in_rect(self.diff_area, x, y) {
                    self.focus = Focus::Diff;
                }
            }
            MouseEventKind::ScrollDown => {
                if in_rect(self.diff_area, x, y) {
                    self.diff_scroll_by(3);
                } else if in_rect(self.tree_area, x, y) {
                    self.focus = Focus::Tree;
                    self.move_selection(1);
                }
            }
            MouseEventKind::ScrollUp => {
                if in_rect(self.diff_area, x, y) {
                    self.diff_scroll = self.diff_scroll.saturating_sub(3);
                } else if in_rect(self.tree_area, x, y) {
                    self.focus = Focus::Tree;
                    self.move_selection(-1);
                }
            }
            _ => {}
        }
    }

    fn diff_scroll_by(&mut self, n: u16) {
        let max = self.diff_content_len.saturating_sub(1) as u16;
        self.diff_scroll = (self.diff_scroll + n).min(max);
    }

    /// Handle a left-click at terminal row `y` inside the tree panel.
    fn tree_click(&mut self, y: u16) {
        let inner = self.tree_inner;
        if y < inner.y || y >= inner.y + inner.height {
            // Clicked the border/title, not a row: just focus the tree.
            self.focus = Focus::Tree;
            return;
        }
        let rel = (y - inner.y) as usize;
        let idx = self.tree_state.offset() + rel;
        if idx >= self.visible.len() {
            self.focus = Focus::Tree;
            return;
        }
        self.focus = Focus::Tree;
        self.selected = idx;
        self.tree_state.select(Some(idx));
        if self.visible[idx].is_dir {
            // Expand only — single-folder collapse is intentionally not offered.
            let path = self.visible[idx].path.clone();
            self.expanded.insert(path);
            self.recompute_visible();
        } else {
            self.load_selected_file();
        }
    }

    /// Move to the next/previous **file** row, skipping directory rows.
    fn move_selection(&mut self, delta: i32) {
        if self.visible.is_empty() {
            return;
        }
        let step: i32 = if delta >= 0 { 1 } else { -1 };
        let len = self.visible.len() as i32;
        let mut i = self.selected as i32 + step;
        while i >= 0 && i < len {
            if !self.visible[i as usize].is_dir {
                self.selected = i as usize;
                self.after_move();
                return;
            }
            i += step;
        }
        // No file in that direction — stay put.
    }

    fn goto_first_file(&mut self) {
        if let Some(i) = self.visible.iter().position(|r| !r.is_dir) {
            self.selected = i;
            self.after_move();
        }
    }

    fn goto_last_file(&mut self) {
        if let Some(i) = self.visible.iter().rposition(|r| !r.is_dir) {
            self.selected = i;
            self.after_move();
        }
    }

    /// Ensure the selection rests on a file, not a directory (e.g. after a
    /// collapse hides the previously-selected file).
    fn snap_to_file(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len() - 1;
        }
        if !self.visible[self.selected].is_dir {
            self.after_move();
            return;
        }
        for i in self.selected..self.visible.len() {
            if !self.visible[i].is_dir {
                self.selected = i;
                self.after_move();
                return;
            }
        }
        for i in (0..self.selected).rev() {
            if !self.visible[i].is_dir {
                self.selected = i;
                self.after_move();
                return;
            }
        }
        // No files visible at all (everything collapsed): keep the cursor.
        self.tree_state.select(Some(self.selected));
    }

    /// After moving the cursor: sync the list widget and auto-open files.
    fn after_move(&mut self) {
        self.tree_state.select(Some(self.selected));
        self.load_selected_file();
    }

    fn activate(&mut self) {
        let Some(row) = self.visible.get(self.selected) else {
            return;
        };
        if row.is_dir {
            // Expand only — single-folder collapse is intentionally not offered.
            let path = row.path.clone();
            self.expanded.insert(path);
            self.recompute_visible();
            self.snap_to_file();
        } else {
            self.load_selected_file();
            self.focus = Focus::Diff;
        }
    }

    fn expand_or_focus(&mut self) {
        let Some(row) = self.visible.get(self.selected) else {
            return;
        };
        if row.is_dir {
            if row.expanded {
                self.move_selection(1); // step into first child
            } else {
                let path = row.path.clone();
                self.expanded.insert(path);
                self.recompute_visible();
            }
        } else {
            self.load_selected_file();
            self.focus = Focus::Diff;
        }
    }

    /// Show/hide the file tree. Focus follows the visible panel.
    fn toggle_tree(&mut self) {
        self.show_tree = !self.show_tree;
        self.focus = if self.show_tree {
            Focus::Tree
        } else {
            Focus::Diff
        };
    }

    /// Toggle the whole tree: if anything is expanded, collapse all; otherwise
    /// expand all.
    fn toggle_collapse_all(&mut self) {
        self.expand_all(self.expanded.is_empty());
    }

    fn expand_all(&mut self, expand: bool) {
        if expand {
            let mut all = HashSet::new();
            tree::all_dir_paths(&self.roots, &mut all);
            self.expanded = all;
        } else {
            self.expanded.clear();
        }
        self.recompute_visible();
        if expand {
            self.snap_to_file();
        }
    }
}
