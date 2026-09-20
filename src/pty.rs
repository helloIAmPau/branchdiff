//! Embeds a *real* terminal program (`$EDITOR`, falling back to `vim`) as a
//! PTY-backed pane rendered in place of the diff panel, instead of the old
//! hand-rolled vim-style widget. The child runs attached to a pseudo-tty; its
//! output is parsed by `vt100` into an in-memory terminal screen, which
//! `tui-term` renders as a normal ratatui widget. Key events are translated
//! back into the raw bytes a real terminal would have sent and written to the
//! PTY, so the child (vim) behaves exactly as it would in any terminal.

use anyhow::Result;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

pub struct PtyEditor {
    pub path: String,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    rows: u16,
    cols: u16,
}

impl PtyEditor {
    /// Spawn `$EDITOR` (falling back to `vim`) on `path`, attached to a PTY
    /// sized `rows`x`cols`.
    pub fn spawn(path: &str, rows: u16, cols: u16) -> Result<Self> {
        let rows = rows.max(1);
        let cols = cols.max(1);

        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
        let mut cmd = CommandBuilder::new(editor);
        cmd.arg(path);
        cmd.env("TERM", "xterm-256color");

        let child = pair.slave.spawn_command(cmd)?;
        // Drop our copy of the slave end now that the child has its own: if we
        // kept one open too, the master reader would never see EOF on exit.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 10_000)));
        let parser_reader = parser.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => parser_reader.lock().unwrap().process(&buf[..n]),
                }
            }
        });

        Ok(Self {
            path: path.to_string(),
            parser,
            writer,
            master: pair.master,
            child,
            rows,
            cols,
        })
    }

    /// Lock and borrow the terminal screen for rendering.
    pub fn parser(&self) -> std::sync::MutexGuard<'_, vt100::Parser> {
        self.parser.lock().unwrap()
    }

    /// Non-blocking check for the child having exited.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Resize both the PTY and the terminal screen model to match the panel.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 || (rows, cols) == (self.rows, self.cols) {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.parser.lock().unwrap().set_size(rows, cols);
    }

    /// Forward a key event to the child as the raw bytes a real terminal
    /// would have sent for it.
    pub fn send_key(&mut self, key: KeyEvent) {
        let bytes = key_to_bytes(key);
        if !bytes.is_empty() {
            let _ = self.writer.write_all(&bytes);
        }
    }
}

/// Translate a crossterm key event into the byte sequence a real terminal
/// would send, so the PTY-side program sees ordinary terminal input.
fn key_to_bytes(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let mut out = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                ctrl_byte(c).map(|b| vec![b]).unwrap_or_default()
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => function_key(n),
        _ => Vec::new(),
    };

    if alt && !out.is_empty() {
        out.insert(0, 0x1b);
    }
    out
}

fn ctrl_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_uppercase();
    match c {
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | ' ' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

fn function_key(n: u8) -> Vec<u8> {
    match n {
        1 => b"\x1bOP".to_vec(),
        2 => b"\x1bOQ".to_vec(),
        3 => b"\x1bOR".to_vec(),
        4 => b"\x1bOS".to_vec(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => Vec::new(),
    }
}
