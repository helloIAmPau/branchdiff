//! A small modal, vim-style text editor used in the right panel.
//!
//! Supports the common subset: Normal / Insert / Command-line / Search and
//! Visual (char + line) modes, basic motions (h/j/k/l, 0/$, w/b, gg/G), edits
//! (i/a/I/A/o/O, x, dd, D), visual-mode d/x/y plus p paste, undo/redo (u /
//! Ctrl-r), `/`-search with n/N, and the `:w` `:q` `:wq` `:q!` and `:<line>`
//! commands.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum EditMode {
    Normal,
    Insert,
    Command,
    /// `/pattern` incremental search entry.
    Search,
    /// Charwise visual selection (`v`).
    Visual,
    /// Linewise visual selection (`V`).
    VisualLine,
}

/// A restorable snapshot of the buffer, used for undo/redo.
#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    cx: usize,
    cy: usize,
}

/// Cap on retained undo history so long editing sessions don't grow unbounded.
const UNDO_LIMIT: usize = 500;

pub struct Editor {
    pub path: String,
    pub lines: Vec<String>,
    /// Cursor column, in characters.
    pub cx: usize,
    /// Cursor line.
    pub cy: usize,
    /// First visible line / column (scroll offsets, updated during draw).
    pub top: usize,
    pub left: usize,
    pub mode: EditMode,
    pub cmd: String,
    pub status: String,
    pub dirty: bool,
    /// Pending operator for two-key sequences like `dd` / `gg`.
    pending: Option<char>,
    /// Anchor of the current visual selection (line, col); `None` outside visual
    /// mode.
    pub anchor: Option<(usize, usize)>,
    /// Undo / redo history of buffer snapshots.
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// Last yanked/deleted text and whether it was linewise (for `p`).
    register: Vec<String>,
    register_linewise: bool,
    /// Last search pattern, reused by `n` / `N`.
    pub last_search: String,
    /// Set when the editor should be closed (`:q`).
    pub quit: bool,
}

fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

/// Char index of the first occurrence of `pat` in `line` at or after char index
/// `from`, or `None`.
fn find_from(line: &str, pat: &str, from: usize) -> Option<usize> {
    let count = line.chars().count();
    if from > count {
        return None;
    }
    let start_byte = char_to_byte(line, from);
    line[start_byte..]
        .find(pat)
        .map(|rel| line[..start_byte + rel].chars().count())
}

/// Char index of the last occurrence of `pat` in `line` that starts strictly
/// before char index `before` (`usize::MAX` = anywhere), or `None`.
fn rfind_before(line: &str, pat: &str, before: usize) -> Option<usize> {
    if pat.is_empty() {
        return None;
    }
    let mut best = None;
    let mut from_byte = 0;
    while let Some(rel) = line[from_byte..].find(pat) {
        let byte = from_byte + rel;
        let cidx = line[..byte].chars().count();
        if cidx < before {
            best = Some(cidx);
        } else {
            break;
        }
        // Advance one char past this match start to find later ones.
        let step = line[byte..].chars().next().map(char::len_utf8).unwrap_or(1);
        from_byte = byte + step;
    }
    best
}

impl Editor {
    pub fn new(path: String, content: &str) -> Self {
        let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
        // A trailing newline yields a spurious empty final element — drop it.
        if content.ends_with('\n') {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        Editor {
            path,
            lines,
            cx: 0,
            cy: 0,
            top: 0,
            left: 0,
            mode: EditMode::Normal,
            cmd: String::new(),
            status: String::new(),
            dirty: false,
            pending: None,
            anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            register: Vec::new(),
            register_linewise: false,
            last_search: String::new(),
            quit: false,
        }
    }

    fn cur_len(&self) -> usize {
        self.lines[self.cy].chars().count()
    }

    fn clamp_cx(&mut self) {
        let max = self.cur_len();
        if self.cx > max {
            self.cx = max;
        }
    }

    // ---- key dispatch ------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        match self.mode {
            EditMode::Normal => self.normal_key(key),
            EditMode::Insert => self.insert_key(key),
            EditMode::Command => self.command_key(key),
            EditMode::Search => self.search_key(key),
            EditMode::Visual | EditMode::VisualLine => self.visual_key(key),
        }
    }

    fn normal_key(&mut self, key: KeyEvent) {
        let prev = self.pending.take();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char(':') => {
                self.mode = EditMode::Command;
                self.cmd.clear();
            }
            KeyCode::Char('/') => {
                self.mode = EditMode::Search;
                self.cmd.clear();
            }
            KeyCode::Char('v') => self.enter_visual(false),
            KeyCode::Char('V') => self.enter_visual(true),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('r') if ctrl => self.redo(),
            KeyCode::Char('p') => self.paste(),
            KeyCode::Char('i') => {
                self.push_undo();
                self.mode = EditMode::Insert;
            }
            KeyCode::Char('a') => {
                self.push_undo();
                self.cx = (self.cx + 1).min(self.cur_len());
                self.mode = EditMode::Insert;
            }
            KeyCode::Char('A') => {
                self.push_undo();
                self.cx = self.cur_len();
                self.mode = EditMode::Insert;
            }
            KeyCode::Char('I') => {
                self.push_undo();
                self.cx = self.first_non_blank();
                self.mode = EditMode::Insert;
            }
            KeyCode::Char('o') => {
                self.push_undo();
                self.lines.insert(self.cy + 1, String::new());
                self.cy += 1;
                self.cx = 0;
                self.mode = EditMode::Insert;
                self.dirty = true;
            }
            KeyCode::Char('O') => {
                self.push_undo();
                self.lines.insert(self.cy, String::new());
                self.cx = 0;
                self.mode = EditMode::Insert;
                self.dirty = true;
            }
            KeyCode::Char('x') => {
                self.push_undo();
                self.delete_char();
            }
            KeyCode::Char('D') => {
                self.push_undo();
                self.delete_to_eol();
            }
            KeyCode::Char('d') => {
                if prev == Some('d') {
                    self.push_undo();
                    self.delete_line();
                } else {
                    self.pending = Some('d');
                }
            }
            _ => {
                self.motion_key(key, prev);
            }
        }
    }

    /// Pure cursor motions shared by Normal and Visual modes. Returns true if the
    /// key was a recognised motion.
    fn motion_key(&mut self, key: KeyEvent, prev: Option<char>) -> bool {
        match key.code {
            KeyCode::Char('h') | KeyCode::Left => self.cx = self.cx.saturating_sub(1),
            KeyCode::Char('l') | KeyCode::Right => {
                if self.cx < self.cur_len() {
                    self.cx += 1;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.cy + 1 < self.lines.len() {
                    self.cy += 1;
                    self.clamp_cx();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.cy > 0 {
                    self.cy -= 1;
                    self.clamp_cx();
                }
            }
            KeyCode::Char('0') | KeyCode::Home => self.cx = 0,
            KeyCode::Char('$') | KeyCode::End => self.cx = self.cur_len(),
            KeyCode::Char('^') => self.cx = self.first_non_blank(),
            KeyCode::Char('w') => self.word_forward(),
            KeyCode::Char('b') => self.word_back(),
            KeyCode::Char('G') => {
                self.cy = self.lines.len() - 1;
                self.clamp_cx();
            }
            KeyCode::Char('g') => {
                if prev == Some('g') {
                    self.cy = 0;
                    self.clamp_cx();
                } else {
                    self.pending = Some('g');
                }
            }
            KeyCode::Char('n') => self.search_next(true),
            KeyCode::Char('N') => self.search_next(false),
            _ => return false,
        }
        true
    }

    fn visual_key(&mut self, key: KeyEvent) {
        let prev = self.pending.take();
        match key.code {
            KeyCode::Esc => {
                self.mode = EditMode::Normal;
                self.anchor = None;
            }
            // Toggle back off (or switch between char/line) with v / V.
            KeyCode::Char('v') => {
                if self.mode == EditMode::Visual {
                    self.mode = EditMode::Normal;
                    self.anchor = None;
                } else {
                    self.mode = EditMode::Visual;
                }
            }
            KeyCode::Char('V') => {
                if self.mode == EditMode::VisualLine {
                    self.mode = EditMode::Normal;
                    self.anchor = None;
                } else {
                    self.mode = EditMode::VisualLine;
                }
            }
            KeyCode::Char('d') | KeyCode::Char('x') => self.delete_selection(),
            KeyCode::Char('y') => self.yank_selection(),
            _ => {
                self.motion_key(key, prev);
            }
        }
    }

    fn insert_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = EditMode::Normal;
                self.cx = self.cx.saturating_sub(1);
            }
            KeyCode::Char(c) => self.insert_char(c),
            KeyCode::Enter => self.newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Tab => {
                for _ in 0..4 {
                    self.insert_char(' ');
                }
            }
            KeyCode::Left => self.cx = self.cx.saturating_sub(1),
            KeyCode::Right => {
                if self.cx < self.cur_len() {
                    self.cx += 1;
                }
            }
            KeyCode::Up => {
                if self.cy > 0 {
                    self.cy -= 1;
                    self.clamp_cx();
                }
            }
            KeyCode::Down => {
                if self.cy + 1 < self.lines.len() {
                    self.cy += 1;
                    self.clamp_cx();
                }
            }
            KeyCode::Home => self.cx = 0,
            KeyCode::End => self.cx = self.cur_len(),
            _ => {}
        }
    }

    fn command_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = EditMode::Normal;
                self.cmd.clear();
            }
            KeyCode::Enter => self.exec_command(),
            KeyCode::Char(c) => self.cmd.push(c),
            KeyCode::Backspace => {
                if self.cmd.is_empty() {
                    self.mode = EditMode::Normal;
                } else {
                    self.cmd.pop();
                }
            }
            _ => {}
        }
    }

    fn search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = EditMode::Normal;
                self.cmd.clear();
            }
            KeyCode::Enter => {
                let pat = self.cmd.trim().to_string();
                self.cmd.clear();
                self.mode = EditMode::Normal;
                if !pat.is_empty() {
                    self.last_search = pat;
                }
                self.search_next(true);
            }
            KeyCode::Char(c) => self.cmd.push(c),
            KeyCode::Backspace => {
                if self.cmd.is_empty() {
                    self.mode = EditMode::Normal;
                } else {
                    self.cmd.pop();
                }
            }
            _ => {}
        }
    }

    // ---- search ------------------------------------------------------------

    /// Move the cursor to the next (or previous) match of the last pattern,
    /// wrapping around the buffer. Sets a status message when nothing is found.
    fn search_next(&mut self, forward: bool) {
        if self.last_search.is_empty() {
            self.status = "No previous search".into();
            return;
        }
        let n = self.lines.len();
        let pat = self.last_search.clone();
        if forward {
            for step in 0..=n {
                let y = (self.cy + step) % n;
                let from = if step == 0 { self.cx + 1 } else { 0 };
                if let Some(cx) = find_from(&self.lines[y], &pat, from) {
                    self.cy = y;
                    self.cx = cx;
                    self.status = String::new();
                    return;
                }
            }
        } else {
            for step in 0..=n {
                let y = (self.cy + n - step) % n;
                let before = if step == 0 { self.cx } else { usize::MAX };
                if let Some(cx) = rfind_before(&self.lines[y], &pat, before) {
                    self.cy = y;
                    self.cx = cx;
                    self.status = String::new();
                    return;
                }
            }
        }
        self.status = format!("Pattern not found: {pat}");
    }

    // ---- undo / redo -------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cx: self.cx,
            cy: self.cy,
        }
    }

    /// Record the current buffer for undo. Call *before* a mutating edit. Clears
    /// the redo stack, since a fresh edit forks history.
    fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > UNDO_LIMIT {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    fn restore(&mut self, s: Snapshot) {
        self.lines = s.lines;
        self.cy = s.cy.min(self.lines.len().saturating_sub(1));
        self.cx = s.cx;
        self.clamp_cx();
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.snapshot());
            self.restore(prev);
            self.dirty = true;
            self.status = "undo".into();
        } else {
            self.status = "Already at oldest change".into();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot());
            self.restore(next);
            self.dirty = true;
            self.status = "redo".into();
        } else {
            self.status = "Already at newest change".into();
        }
    }

    // ---- visual mode -------------------------------------------------------

    fn enter_visual(&mut self, linewise: bool) {
        self.anchor = Some((self.cy, self.cx));
        self.mode = if linewise {
            EditMode::VisualLine
        } else {
            EditMode::Visual
        };
    }

    /// The current selection as a normalized (start, end) pair with start <= end,
    /// or `None` when not in visual mode.
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anc = self.anchor?;
        let cur = (self.cy, self.cx);
        Some(if anc <= cur { (anc, cur) } else { (cur, anc) })
    }

    /// Extract the selected text (charwise, end inclusive) as register lines.
    fn charwise_slice(&self, start: (usize, usize), end: (usize, usize)) -> Vec<String> {
        let (sy, sx) = start;
        let (ey, ex) = end;
        if sy == ey {
            let chars: Vec<char> = self.lines[sy].chars().collect();
            let e = (ex + 1).min(chars.len());
            let s = sx.min(e);
            vec![chars[s..e].iter().collect()]
        } else {
            let mut out = Vec::new();
            let first: Vec<char> = self.lines[sy].chars().collect();
            out.push(first[sx.min(first.len())..].iter().collect());
            for y in sy + 1..ey {
                out.push(self.lines[y].clone());
            }
            let last: Vec<char> = self.lines[ey].chars().collect();
            let e = (ex + 1).min(last.len());
            out.push(last[..e].iter().collect());
            out
        }
    }

    /// Remove the charwise selection (end inclusive), joining the boundary lines.
    fn remove_charwise(&mut self, start: (usize, usize), end: (usize, usize)) {
        let (sy, sx) = start;
        let (ey, ex) = end;
        if sy == ey {
            let count = self.lines[sy].chars().count();
            let s = char_to_byte(&self.lines[sy], sx);
            let e = char_to_byte(&self.lines[sy], (ex + 1).min(count));
            self.lines[sy].replace_range(s..e, "");
        } else {
            let s = char_to_byte(&self.lines[sy], sx);
            let head = self.lines[sy][..s].to_string();
            let ecount = self.lines[ey].chars().count();
            let e = char_to_byte(&self.lines[ey], (ex + 1).min(ecount));
            let tail = self.lines[ey][e..].to_string();
            self.lines.drain(sy..=ey);
            self.lines.insert(sy, format!("{head}{tail}"));
        }
    }

    fn yank_selection(&mut self) {
        let Some((start, end)) = self.selection() else {
            return;
        };
        if self.mode == EditMode::VisualLine {
            self.register = self.lines[start.0..=end.0].to_vec();
            self.register_linewise = true;
            self.status = format!("{} lines yanked", self.register.len());
        } else {
            self.register = self.charwise_slice(start, end);
            self.register_linewise = false;
            self.status = "yanked".into();
        }
        self.cy = start.0;
        self.cx = start.1;
        self.clamp_cx();
        self.mode = EditMode::Normal;
        self.anchor = None;
    }

    fn delete_selection(&mut self) {
        let Some((start, end)) = self.selection() else {
            return;
        };
        self.push_undo();
        if self.mode == EditMode::VisualLine {
            self.register = self.lines[start.0..=end.0].to_vec();
            self.register_linewise = true;
            self.lines.drain(start.0..=end.0);
            if self.lines.is_empty() {
                self.lines.push(String::new());
            }
            self.cy = start.0.min(self.lines.len() - 1);
            self.cx = 0;
        } else {
            self.register = self.charwise_slice(start, end);
            self.register_linewise = false;
            self.remove_charwise(start, end);
            self.cy = start.0;
            self.cx = start.1;
            self.clamp_cx();
        }
        self.dirty = true;
        self.mode = EditMode::Normal;
        self.anchor = None;
    }

    /// Insert `text` (as lines) at (`cy`, `col`), returning the cursor position
    /// just past the inserted text.
    fn insert_lines_at(&mut self, cy: usize, col: usize, text: &[String]) -> (usize, usize) {
        if text.is_empty() {
            return (cy, col);
        }
        let byte = char_to_byte(&self.lines[cy], col);
        let tail = self.lines[cy].split_off(byte);
        if text.len() == 1 {
            self.lines[cy].push_str(&text[0]);
            let end_col = col + text[0].chars().count();
            self.lines[cy].push_str(&tail);
            (cy, end_col)
        } else {
            self.lines[cy].push_str(&text[0]);
            let mut idx = cy;
            for l in &text[1..text.len() - 1] {
                idx += 1;
                self.lines.insert(idx, l.clone());
            }
            idx += 1;
            let last = &text[text.len() - 1];
            let end_col = last.chars().count();
            self.lines.insert(idx, format!("{last}{tail}"));
            (idx, end_col)
        }
    }

    /// Paste the register after the cursor (vim `p`).
    fn paste(&mut self) {
        if self.register.is_empty() {
            return;
        }
        self.push_undo();
        let reg = self.register.clone();
        if self.register_linewise {
            let at = self.cy + 1;
            for (k, l) in reg.iter().enumerate() {
                self.lines.insert(at + k, l.clone());
            }
            self.cy = at;
            self.cx = 0;
        } else {
            let col = if self.cur_len() == 0 { 0 } else { self.cx + 1 };
            let (ny, nx) = self.insert_lines_at(self.cy, col, &reg);
            self.cy = ny;
            self.cx = nx.saturating_sub(1);
        }
        self.dirty = true;
    }

    // ---- editing primitives ------------------------------------------------

    fn first_non_blank(&self) -> usize {
        self.lines[self.cy]
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0)
    }

    fn insert_char(&mut self, ch: char) {
        let byte = char_to_byte(&self.lines[self.cy], self.cx);
        self.lines[self.cy].insert(byte, ch);
        self.cx += 1;
        self.dirty = true;
    }

    fn newline(&mut self) {
        let byte = char_to_byte(&self.lines[self.cy], self.cx);
        let tail = self.lines[self.cy].split_off(byte);
        self.lines.insert(self.cy + 1, tail);
        self.cy += 1;
        self.cx = 0;
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.cx > 0 {
            let start = char_to_byte(&self.lines[self.cy], self.cx - 1);
            let end = char_to_byte(&self.lines[self.cy], self.cx);
            self.lines[self.cy].replace_range(start..end, "");
            self.cx -= 1;
        } else if self.cy > 0 {
            let cur = self.lines.remove(self.cy);
            self.cy -= 1;
            self.cx = self.cur_len();
            self.lines[self.cy].push_str(&cur);
        }
        self.dirty = true;
    }

    fn delete_char(&mut self) {
        if self.cx < self.cur_len() {
            let start = char_to_byte(&self.lines[self.cy], self.cx);
            let end = char_to_byte(&self.lines[self.cy], self.cx + 1);
            self.lines[self.cy].replace_range(start..end, "");
            let len = self.cur_len();
            if self.cx >= len && len > 0 {
                self.cx = len - 1;
            }
            self.dirty = true;
        }
    }

    fn delete_to_eol(&mut self) {
        let byte = char_to_byte(&self.lines[self.cy], self.cx);
        self.lines[self.cy].truncate(byte);
        self.clamp_cx();
        self.dirty = true;
    }

    fn delete_line(&mut self) {
        if self.lines.len() > 1 {
            self.lines.remove(self.cy);
            if self.cy >= self.lines.len() {
                self.cy = self.lines.len() - 1;
            }
        } else {
            self.lines[0].clear();
        }
        self.cx = 0;
        self.dirty = true;
    }

    fn word_forward(&mut self) {
        let chars: Vec<char> = self.lines[self.cy].chars().collect();
        let n = chars.len();
        let mut i = self.cx;
        while i < n && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= n && self.cy + 1 < self.lines.len() {
            self.cy += 1;
            self.cx = 0;
        } else {
            self.cx = i.min(n);
        }
    }

    fn word_back(&mut self) {
        if self.cx == 0 {
            if self.cy > 0 {
                self.cy -= 1;
                self.cx = self.cur_len();
            }
            return;
        }
        let chars: Vec<char> = self.lines[self.cy].chars().collect();
        let mut i = self.cx.saturating_sub(1);
        while i > 0 && chars[i].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.cx = i;
    }

    // ---- commands ----------------------------------------------------------

    fn exec_command(&mut self) {
        let cmd = self.cmd.trim().to_string();
        // `:<number>` jumps to that (1-based) line.
        if let Ok(n) = cmd.parse::<usize>() {
            let target = n.saturating_sub(1).min(self.lines.len() - 1);
            self.cy = target;
            self.clamp_cx();
            self.status = String::new();
            self.cmd.clear();
            self.mode = EditMode::Normal;
            return;
        }
        match cmd.as_str() {
            "w" => {
                self.save();
            }
            "q" => {
                if self.dirty {
                    self.status = "No write since last change (add ! to override)".into();
                } else {
                    self.quit = true;
                }
            }
            "q!" => self.quit = true,
            "wq" | "x" => {
                if self.save() {
                    self.quit = true;
                }
            }
            other => self.status = format!("Not an editor command: {other}"),
        }
        self.cmd.clear();
        self.mode = EditMode::Normal;
    }

    fn save(&mut self) -> bool {
        let content = self.lines.join("\n") + "\n";
        match std::fs::write(&self.path, content) {
            Ok(()) => {
                self.dirty = false;
                self.status = format!("\"{}\" {}L written", self.path, self.lines.len());
                true
            }
            Err(e) => {
                self.status = format!("write failed: {e}");
                false
            }
        }
    }

    /// A short label for the current mode, shown in the status line.
    pub fn mode_label(&self) -> &'static str {
        match self.mode {
            EditMode::Normal => "NORMAL",
            EditMode::Insert => "INSERT",
            EditMode::Command => "COMMAND",
            EditMode::Search => "SEARCH",
            EditMode::Visual => "VISUAL",
            EditMode::VisualLine => "V-LINE",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyCode;

    fn ed(content: &str) -> Editor {
        Editor::new("test.txt".to_string(), content)
    }

    /// Feed a sequence of character keys.
    fn typ(ed: &mut Editor, keys: &str) {
        for c in keys.chars() {
            ed.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    fn press(ed: &mut Editor, code: KeyCode) {
        ed.handle_key(KeyEvent::from(code));
    }

    #[test]
    fn new_splits_lines_and_drops_trailing_newline() {
        let e = ed("one\ntwo\n");
        assert_eq!(e.lines, vec!["one", "two"]);
        assert_eq!(e.cy, 0);
        assert_eq!(e.cx, 0);
        assert!(!e.dirty);
    }

    #[test]
    fn new_empty_content_has_one_blank_line() {
        let e = ed("");
        assert_eq!(e.lines, vec![""]);
    }

    #[test]
    fn insert_mode_types_text() {
        let mut e = ed("");
        typ(&mut e, "i");
        assert_eq!(e.mode, EditMode::Insert);
        typ(&mut e, "hello");
        assert_eq!(e.lines[0], "hello");
        assert_eq!(e.cx, 5);
        assert!(e.dirty);
        press(&mut e, KeyCode::Esc);
        assert_eq!(e.mode, EditMode::Normal);
        // Esc steps the cursor back one, vim-style.
        assert_eq!(e.cx, 4);
    }

    #[test]
    fn append_starts_after_cursor() {
        let mut e = ed("ab");
        typ(&mut e, "a"); // append
        assert_eq!(e.cx, 1);
        typ(&mut e, "X");
        assert_eq!(e.lines[0], "aXb");
    }

    #[test]
    fn open_line_below_and_above() {
        let mut e = ed("line");
        typ(&mut e, "o");
        assert_eq!(e.lines, vec!["line", ""]);
        assert_eq!(e.cy, 1);
        press(&mut e, KeyCode::Esc);
        typ(&mut e, "O");
        assert_eq!(e.cy, 1);
        assert_eq!(e.lines, vec!["line", "", ""]);
    }

    #[test]
    fn horizontal_motions_clamp() {
        let mut e = ed("abc");
        typ(&mut e, "l"); // 0 -> 1
        typ(&mut e, "l"); // 1 -> 2
        typ(&mut e, "l"); // clamps at len-1 in normal mode boundary (cur_len)
        assert!(e.cx <= e.cur_len());
        typ(&mut e, "0");
        assert_eq!(e.cx, 0);
        typ(&mut e, "$");
        assert_eq!(e.cx, 3);
    }

    #[test]
    fn vertical_motion_clamps_column() {
        let mut e = ed("longline\nhi");
        typ(&mut e, "$"); // cx = 8
        typ(&mut e, "j"); // move to short line -> clamp
        assert_eq!(e.cy, 1);
        assert_eq!(e.cx, 2);
    }

    #[test]
    fn gg_and_G_jump_to_ends() {
        let mut e = ed("a\nb\nc");
        typ(&mut e, "G");
        assert_eq!(e.cy, 2);
        typ(&mut e, "gg");
        assert_eq!(e.cy, 0);
    }

    #[test]
    fn x_deletes_char_under_cursor() {
        let mut e = ed("abc");
        typ(&mut e, "x");
        assert_eq!(e.lines[0], "bc");
        assert!(e.dirty);
    }

    #[test]
    fn capital_d_deletes_to_end_of_line() {
        let mut e = ed("hello world");
        typ(&mut e, "l"); // cx=1
        typ(&mut e, "D");
        assert_eq!(e.lines[0], "h");
    }

    #[test]
    fn dd_deletes_line_and_last_line_is_cleared_not_removed() {
        let mut e = ed("a\nb");
        typ(&mut e, "dd");
        assert_eq!(e.lines, vec!["b"]);
        typ(&mut e, "dd");
        assert_eq!(e.lines, vec![""]); // never drops below one line
    }

    #[test]
    fn word_motions() {
        let mut e = ed("foo bar baz");
        typ(&mut e, "w");
        assert_eq!(e.cx, 4); // start of "bar"
        typ(&mut e, "w");
        assert_eq!(e.cx, 8); // start of "baz"
        typ(&mut e, "b");
        assert_eq!(e.cx, 4); // back to "bar"
    }

    #[test]
    fn backspace_joins_lines() {
        let mut e = ed("ab\ncd");
        typ(&mut e, "j"); // cy=1
        typ(&mut e, "i"); // insert mode at col 0
        press(&mut e, KeyCode::Backspace);
        assert_eq!(e.lines, vec!["abcd"]);
        assert_eq!(e.cy, 0);
        assert_eq!(e.cx, 2);
    }

    #[test]
    fn enter_splits_line() {
        let mut e = ed("abcd");
        typ(&mut e, "ll"); // cx=2
        typ(&mut e, "i");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.lines, vec!["ab", "cd"]);
    }

    #[test]
    fn unicode_insert_and_delete_by_char() {
        let mut e = ed("héllo");
        typ(&mut e, "x"); // delete 'h'
        assert_eq!(e.lines[0], "éllo");
        typ(&mut e, "x"); // delete multi-byte 'é'
        assert_eq!(e.lines[0], "llo");
    }

    #[test]
    fn quit_blocked_while_dirty_then_forced() {
        let mut e = ed("x");
        typ(&mut e, "i");
        typ(&mut e, "y");
        press(&mut e, KeyCode::Esc);
        assert!(e.dirty);
        // :q refuses on unsaved changes
        run_cmd(&mut e, "q");
        assert!(!e.quit);
        assert!(e.status.contains("No write"));
        // :q! forces
        run_cmd(&mut e, "q!");
        assert!(e.quit);
    }

    #[test]
    fn write_persists_and_clears_dirty() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("branchdiff_ed_{}.txt", std::process::id()));
        let mut e = Editor::new(path.to_string_lossy().to_string(), "old\n");
        typ(&mut e, "A"); // append at end of line
        typ(&mut e, "!");
        press(&mut e, KeyCode::Esc);
        assert!(e.dirty);
        run_cmd(&mut e, "w");
        assert!(!e.dirty);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "old!\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_command_reports_error() {
        let mut e = ed("x");
        run_cmd(&mut e, "bogus");
        assert!(e.status.contains("Not an editor command"));
        assert_eq!(e.mode, EditMode::Normal);
    }

    /// Enter command mode, type the command, and press Enter.
    fn run_cmd(ed: &mut Editor, cmd: &str) {
        ed.handle_key(KeyEvent::from(KeyCode::Char(':')));
        for c in cmd.chars() {
            ed.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        ed.handle_key(KeyEvent::from(KeyCode::Enter));
    }

    /// Enter search mode, type the pattern, and press Enter.
    fn run_search(ed: &mut Editor, pat: &str) {
        ed.handle_key(KeyEvent::from(KeyCode::Char('/')));
        for c in pat.chars() {
            ed.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        ed.handle_key(KeyEvent::from(KeyCode::Enter));
    }

    fn ctrl(ed: &mut Editor, c: char) {
        ed.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    #[test]
    fn visual_char_delete_removes_inclusive_range() {
        let mut e = ed("hello world");
        typ(&mut e, "v"); // anchor at col 0
        assert_eq!(e.mode, EditMode::Visual);
        typ(&mut e, "llll"); // cursor now on 'o' (col 4)
        typ(&mut e, "d");
        assert_eq!(e.lines[0], " world"); // "hello" removed, end inclusive
        assert_eq!(e.mode, EditMode::Normal);
        assert_eq!((e.cy, e.cx), (0, 0));
    }

    #[test]
    fn visual_char_delete_spans_lines() {
        let mut e = ed("abc\ndef");
        typ(&mut e, "ll"); // col 2 ('c')
        typ(&mut e, "v"); // anchor (0,2)
        typ(&mut e, "j"); // cursor (1,2) -> clamps? "def" len 3 so cx=2 ('f')
        typ(&mut e, "d"); // remove "c\ndef"[..='f'] joining
        assert_eq!(e.lines, vec!["ab"]);
    }

    #[test]
    fn visual_line_delete_removes_whole_lines() {
        let mut e = ed("a\nb\nc");
        typ(&mut e, "V"); // linewise, anchor line 0
        assert_eq!(e.mode, EditMode::VisualLine);
        typ(&mut e, "j"); // extend to line 1
        typ(&mut e, "d");
        assert_eq!(e.lines, vec!["c"]);
    }

    #[test]
    fn visual_line_yank_and_paste() {
        let mut e = ed("a\nb\nc");
        typ(&mut e, "V"); // select line 0
        typ(&mut e, "y"); // yank it linewise, back to normal
        assert_eq!(e.mode, EditMode::Normal);
        typ(&mut e, "p"); // paste below line 0
        assert_eq!(e.lines, vec!["a", "a", "b", "c"]);
        assert_eq!(e.cy, 1);
    }

    #[test]
    fn visual_char_yank_and_paste() {
        let mut e = ed("abc");
        typ(&mut e, "vll"); // select "abc"
        typ(&mut e, "y"); // yank, cursor back to (0,0)
        assert_eq!((e.cy, e.cx), (0, 0));
        typ(&mut e, "p"); // paste after char 'a'
        assert_eq!(e.lines[0], "aabcbc");
    }

    #[test]
    fn undo_and_redo_single_edit() {
        let mut e = ed("hello");
        typ(&mut e, "x"); // -> "ello"
        assert_eq!(e.lines[0], "ello");
        typ(&mut e, "u"); // undo
        assert_eq!(e.lines[0], "hello");
        ctrl(&mut e, 'r'); // redo
        assert_eq!(e.lines[0], "ello");
    }

    #[test]
    fn undo_groups_an_insert_session() {
        let mut e = ed("");
        typ(&mut e, "i");
        typ(&mut e, "abc");
        press(&mut e, KeyCode::Esc);
        assert_eq!(e.lines[0], "abc");
        typ(&mut e, "u"); // the whole insert is one undo step
        assert_eq!(e.lines[0], "");
    }

    #[test]
    fn undo_restores_deleted_lines() {
        let mut e = ed("a\nb\nc");
        typ(&mut e, "Vjd"); // delete first two lines
        assert_eq!(e.lines, vec!["c"]);
        typ(&mut e, "u");
        assert_eq!(e.lines, vec!["a", "b", "c"]);
    }

    #[test]
    fn search_jumps_and_repeats_wrapping() {
        let mut e = ed("foo\nbar\nfoo baz");
        run_search(&mut e, "foo");
        assert_eq!((e.cy, e.cx), (2, 0)); // first match after the cursor
        typ(&mut e, "n"); // wraps back to the top match
        assert_eq!((e.cy, e.cx), (0, 0));
        typ(&mut e, "N"); // previous wraps forward again
        assert_eq!((e.cy, e.cx), (2, 0));
    }

    #[test]
    fn search_reports_missing_pattern() {
        let mut e = ed("alpha\nbeta");
        run_search(&mut e, "zzz");
        assert!(e.status.contains("not found"));
        assert_eq!((e.cy, e.cx), (0, 0)); // cursor unchanged
    }

    #[test]
    fn goto_line_command_jumps() {
        let mut e = ed("a\nb\nc\nd");
        run_cmd(&mut e, "3");
        assert_eq!(e.cy, 2);
        run_cmd(&mut e, "999"); // clamps to last line
        assert_eq!(e.cy, 3);
        run_cmd(&mut e, "0"); // 0 -> first line
        assert_eq!(e.cy, 0);
    }
}
