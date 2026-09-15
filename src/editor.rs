//! A small modal, vim-style text editor used in the right panel.
//!
//! Supports the common subset: Normal / Insert / Command-line modes, basic
//! motions (h/j/k/l, 0/$, w/b, gg/G), edits (i/a/I/A/o/O, x, dd, D), and the
//! `:w` `:q` `:wq` `:q!` commands.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum EditMode {
    Normal,
    Insert,
    Command,
}

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
    /// Set when the editor should be closed (`:q`).
    pub quit: bool,
}

fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
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
        }
    }

    fn normal_key(&mut self, key: KeyEvent) {
        let prev = self.pending.take();
        match key.code {
            KeyCode::Char(':') => {
                self.mode = EditMode::Command;
                self.cmd.clear();
            }
            KeyCode::Char('i') => self.mode = EditMode::Insert,
            KeyCode::Char('a') => {
                self.cx = (self.cx + 1).min(self.cur_len());
                self.mode = EditMode::Insert;
            }
            KeyCode::Char('A') => {
                self.cx = self.cur_len();
                self.mode = EditMode::Insert;
            }
            KeyCode::Char('I') => {
                self.cx = self.first_non_blank();
                self.mode = EditMode::Insert;
            }
            KeyCode::Char('o') => {
                self.lines.insert(self.cy + 1, String::new());
                self.cy += 1;
                self.cx = 0;
                self.mode = EditMode::Insert;
                self.dirty = true;
            }
            KeyCode::Char('O') => {
                self.lines.insert(self.cy, String::new());
                self.cx = 0;
                self.mode = EditMode::Insert;
                self.dirty = true;
            }
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
            KeyCode::Char('x') => self.delete_char(),
            KeyCode::Char('D') => self.delete_to_eol(),
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
            KeyCode::Char('d') => {
                if prev == Some('d') {
                    self.delete_line();
                } else {
                    self.pending = Some('d');
                }
            }
            _ => {}
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
        }
    }
}
