//! Syntax highlighting via `syntect`. Produces a per-character foreground colour
//! for a line of code; the diff renderer combines these with its own
//! (delta-style) backgrounds.

use ratatui::style::Color;
use std::io::Cursor;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// GitHub Dark theme, bundled so it needs no runtime file.
const GITHUB_DARK: &str = include_str!("../assets/github-dark.tmTheme");

pub struct Highlighter {
    ss: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new() -> Self {
        let ss = SyntaxSet::load_defaults_newlines();
        let theme = ThemeSet::load_from_reader(&mut Cursor::new(GITHUB_DARK))
            .expect("embedded GitHub Dark theme is valid");
        Highlighter { ss, theme }
    }

    /// Pick a syntax by file extension, falling back to plain text.
    pub fn syntax_for(&self, path: &str) -> &SyntaxReference {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        self.ss
            .find_syntax_by_extension(ext)
            .or_else(|| self.ss.find_syntax_by_token(ext))
            .unwrap_or_else(|| self.ss.find_syntax_plain_text())
    }

    /// One foreground colour per character of `text`. Lines are highlighted
    /// independently (a fresh state per line): multi-line constructs like block
    /// comments won't carry across hunks, but single-line tokens are correct.
    pub fn line_colors(&self, syntax: &SyntaxReference, text: &str) -> Vec<Color> {
        let mut h = HighlightLines::new(syntax, &self.theme);
        let mut out: Vec<Color> = Vec::with_capacity(text.len());
        if let Ok(ranges) = h.highlight_line(text, &self.ss) {
            for (style, piece) in ranges {
                let fg = style.foreground;
                let color = Color::Rgb(fg.r, fg.g, fg.b);
                for _ in piece.chars() {
                    out.push(color);
                }
            }
        }
        out
    }
}
