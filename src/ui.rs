//! All rendering: title bar, file tree, diff panel (inline + side-by-side),
//! and the help/status footer.

use crate::app::{App, Focus};
use crate::diff::{self, FileDiff, LineKind, SideRow};
use crate::editor::EditMode;
use crate::highlight::Highlighter;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;
use syntect::parsing::SyntaxReference;

// Foreground accents (title branch names, tree status letters), Claude Code style.
const ADD: Color = Color::Rgb(63, 185, 80); // #3fb950 green
const DEL: Color = Color::Rgb(248, 81, 73); // #f85149 red
const CTX: Color = Color::Gray;
const GUT: Color = Color::DarkGray;

pub fn draw(f: &mut Frame, app: &mut App) {
    let root = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Min(1),    // body
        Constraint::Length(1), // footer
    ])
    .split(f.area());

    draw_title(f, root[0], app);

    let right = if app.show_tree {
        let body = Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)])
            .split(root[1]);
        app.tree_area = body[0];
        draw_tree(f, body[0], app);
        body[1]
    } else {
        // Tree hidden: the diff/editor takes the full width.
        app.tree_area = Rect::default();
        app.tree_inner = Rect::default();
        root[1]
    };

    app.diff_area = right;
    if app.editor.is_some() {
        draw_editor(f, right, app);
    } else {
        draw_diff(f, right, app);
    }
    draw_footer(f, root[2], app);
}

fn draw_title(f: &mut Frame, area: Rect, app: &App) {
    let line = Line::from(vec![
        Span::styled(" branchdiff ", Style::default().fg(Color::Black).bg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(app.branch.clone(), Style::default().fg(DEL).add_modifier(Modifier::BOLD)),
        Span::styled(" … ", Style::default().fg(CTX)),
        Span::styled(app.current_branch.clone(), Style::default().fg(ADD).add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("   ({} files)", app.files.len()),
            Style::default().fg(CTX),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn status_style(status: char) -> (char, Color) {
    match status {
        'A' => ('A', ADD),
        'M' => ('M', Color::Yellow),
        'D' => ('D', DEL),
        'R' => ('R', Color::Magenta),
        'C' => ('C', Color::Blue),
        _ => ('?', CTX),
    }
}

fn draw_tree(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Tree;
    let border = if focused { Color::Cyan } else { Color::DarkGray };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            " Files ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ));

    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|r| {
            let indent = "  ".repeat(r.depth);
            if r.is_dir {
                let arrow = if r.expanded { "▾" } else { "▸" };
                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(
                        format!("{arrow} {}/", r.name),
                        Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                    ),
                ]))
            } else {
                let (letter, color) = status_style(r.status.unwrap_or('?'));
                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::raw("  "),
                    Span::styled(
                        format!("{letter} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(r.name.clone(), Style::default().fg(Color::White)),
                ]))
            }
        })
        .collect();

    app.tree_inner = block.inner(area);

    let highlight = if focused {
        Style::default().bg(Color::Rgb(40, 60, 90)).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::Rgb(40, 40, 40))
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(highlight)
        .highlight_symbol("");

    f.render_stateful_widget(list, area, &mut app.tree_state);
}

fn draw_diff(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Diff;
    let border = if focused { Color::Cyan } else { Color::DarkGray };

    let title = match app.current_file {
        Some(idx) => {
            let file = &app.files[idx];
            match &file.old_path {
                Some(old) if old != &file.path => format!(" {} → {} ", old, file.path),
                _ => format!(" {} ", file.path),
            }
        }
        None if app.files.is_empty() => " (no differences) ".to_string(),
        None => " (no file selected) ".to_string(),
    };
    let mode = if app.side_by_side { "split" } else { "unified" };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            title,
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))
        .title(
            Line::from(Span::styled(
                format!(" {mode} "),
                Style::default().fg(Color::Black).bg(Color::Cyan),
            ))
            .right_aligned(),
        );

    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines: Vec<Line<'static>> = match app.current_file {
        Some(idx) => {
            let fd = app.diff_cache.get(&idx);
            let syntax = app.highlighter.syntax_for(&app.files[idx].path);
            let hl = &app.highlighter;
            match fd {
                Some(fd) if app.side_by_side => {
                    build_side(fd, inner.width, app.diff_hscroll, hl, syntax)
                }
                Some(fd) => build_inline(fd, inner.width, app.diff_hscroll, hl, syntax),
                None => vec![Line::from("Loading…")],
            }
        }
        None if app.files.is_empty() => message_lines(
            &format!(
                "No differences between {} and the working tree.",
                app.branch
            ),
            CTX,
        ),
        None => vec![Line::from("Select a file in the tree to view its diff.")],
    };

    // Record geometry so the key handler can clamp scrolling / paging.
    app.diff_content_len = lines.len();
    app.diff_viewport = inner.height;
    let max_scroll = lines.len().saturating_sub(1) as u16;
    if app.diff_scroll > max_scroll {
        app.diff_scroll = max_scroll;
    }

    let para = Paragraph::new(lines).scroll((app.diff_scroll, 0));
    f.render_widget(para, inner);
}

fn mode_color(mode: EditMode) -> Color {
    match mode {
        EditMode::Normal => Color::Blue,
        EditMode::Insert => Color::Green,
        EditMode::Command | EditMode::Search => Color::Magenta,
        EditMode::Visual | EditMode::VisualLine => Color::Rgb(215, 153, 33), // amber
    }
}

/// Background applied to the current visual selection.
const SEL_BG: Color = Color::Rgb(58, 74, 110);

/// For a given editor line, return the `(base_bg, emph_range)` to apply so the
/// visual selection is highlighted. Linewise selections colour the whole row;
/// charwise selections colour just the covered char range.
fn sel_line_style(
    sel: Option<((usize, usize), (usize, usize))>,
    linewise: bool,
    y: usize,
    len: usize,
) -> (Option<Color>, Option<(usize, usize)>) {
    let Some((s, e)) = sel else {
        return (None, None);
    };
    if y < s.0 || y > e.0 {
        return (None, None);
    }
    if linewise {
        return (Some(SEL_BG), None);
    }
    let start = if y == s.0 { s.1 } else { 0 };
    let end = if y == e.0 { (e.1 + 1).min(len) } else { len };
    if start >= end {
        return (None, None);
    }
    (None, Some((start, end)))
}

fn draw_editor(f: &mut Frame, area: Rect, app: &mut App) {
    let hl = &app.highlighter;
    let ed = match app.editor.as_mut() {
        Some(ed) => ed,
        None => return,
    };
    let syntax = hl.syntax_for(&ed.path);

    let dirty = if ed.dirty { " [+]" } else { "" };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(Span::styled(
            format!(" {}{} ", ed.path, dirty),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))
        .title(
            Line::from(Span::styled(
                " EDIT ",
                Style::default().fg(Color::Black).bg(Color::Green),
            ))
            .right_aligned(),
        );

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 || inner.width < 4 {
        return;
    }

    let text_h = inner.height.saturating_sub(1) as usize; // reserve last row for status
    let total = ed.lines.len();
    let gutter = total.to_string().len().max(3) + 1; // digits + trailing space
    let text_w = (inner.width as usize).saturating_sub(gutter + 1).max(1);

    // Keep the cursor within the viewport (vertical + horizontal scroll).
    if ed.cy < ed.top {
        ed.top = ed.cy;
    } else if ed.cy >= ed.top + text_h {
        ed.top = ed.cy + 1 - text_h;
    }
    if ed.cx < ed.left {
        ed.left = ed.cx;
    } else if ed.cx >= ed.left + text_w {
        ed.left = ed.cx + 1 - text_w;
    }

    let sel = ed.selection();
    let linewise = ed.mode == EditMode::VisualLine;

    let mut out: Vec<Line<'static>> = Vec::new();
    for i in ed.top..(ed.top + text_h).min(total) {
        let lineno = format!("{:>w$} ", i + 1, w = gutter - 1);
        let display = ed.lines[i].replace('\t', "    ");
        let fgs = hl.line_colors(syntax, &display);
        let num_style = if i == ed.cy {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(GUT)
        };
        let (sel_base, sel_emph) = sel_line_style(sel, linewise, i, display.chars().count());
        let mut spans = vec![Span::styled(lineno, num_style)];
        spans.extend(fill_body(
            &display,
            sel_emph,
            sel_base,
            SEL_BG,
            &fgs,
            TEXT,
            ed.left,
            text_w,
        ));
        out.push(Line::from(spans));
    }

    let text_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: text_h as u16,
    };
    f.render_widget(Paragraph::new(out), text_area);

    // Status / command line along the bottom row of the panel.
    let status_y = inner.y + inner.height - 1;
    let status_area = Rect {
        x: inner.x,
        y: status_y,
        width: inner.width,
        height: 1,
    };
    let status_line = if ed.mode == EditMode::Command || ed.mode == EditMode::Search {
        let prefix = if ed.mode == EditMode::Search { '/' } else { ':' };
        Line::from(Span::styled(
            format!("{prefix}{}", ed.cmd),
            Style::default().fg(Color::White),
        ))
    } else {
        Line::from(vec![
            Span::styled(
                format!(" {} ", ed.mode_label()),
                Style::default()
                    .fg(Color::Black)
                    .bg(mode_color(ed.mode))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}:{}  ", ed.cy + 1, ed.cx + 1),
                Style::default().fg(CTX),
            ),
            Span::styled(ed.status.clone(), Style::default().fg(CTX)),
        ])
    };
    f.render_widget(
        Paragraph::new(status_line).style(Style::default().bg(Color::Rgb(25, 25, 30))),
        status_area,
    );

    // Real terminal cursor.
    let right_edge = inner.x + inner.width - 1;
    if ed.mode == EditMode::Command || ed.mode == EditMode::Search {
        let cx = inner.x + 1 + ed.cmd.chars().count() as u16;
        f.set_cursor_position((cx.min(right_edge), status_y));
    } else {
        let sx = inner.x + gutter as u16 + (ed.cx - ed.left) as u16;
        let sy = inner.y + (ed.cy - ed.top) as u16;
        if sy < status_y {
            f.set_cursor_position((sx.min(right_edge), sy));
        }
    }
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let key = |k: &str| Span::styled(k.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    let sep = || Span::styled("  ", Style::default());
    let lbl = |t: &str| Span::styled(t.to_string(), Style::default().fg(CTX));

    if app.editor.is_some() {
        let line = Line::from(vec![
            key(" i/a/o"),
            lbl("insert"),
            sep(),
            key("v/V"),
            lbl("visual"),
            sep(),
            key("x/d/y/p"),
            lbl("cut/yank/paste"),
            sep(),
            key("u"),
            lbl("undo"),
            sep(),
            key("^r"),
            lbl("redo"),
            sep(),
            key("/n"),
            lbl("search"),
            sep(),
            key(":N"),
            lbl("goto line"),
            sep(),
            key(":w :q"),
            lbl("save/quit"),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(Color::Rgb(20, 30, 20))),
            area,
        );
        return;
    }

    let (nav_hint, mode_key) = match app.focus {
        Focus::Tree => ("↑↓ move · →/⏎ open", "Tab diff"),
        Focus::Diff => ("↑↓ scroll · ←→ pan · PgUp/PgDn page", "Tab/Esc tree"),
    };

    let line = Line::from(vec![
        key(" "),
        lbl(nav_hint),
        sep(),
        key("[Tab]"),
        lbl(mode_key),
        sep(),
        key("[s]"),
        lbl(if app.side_by_side { "unified" } else { "split" }),
        sep(),
        key("[t]"),
        lbl(if app.show_tree { "hide tree" } else { "show tree" }),
        sep(),
        key("[e]"),
        lbl("edit"),
        sep(),
        key("[C]"),
        lbl("collapse/expand all"),
        sep(),
        key("[q]"),
        lbl("quit"),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::Rgb(25, 25, 30))),
        area,
    );
}

// ---- diff line builders (git-delta–style) ----------------------------------

// Claude Code style green/red diff palette: full-width row backgrounds, brighter
// emphasis over the exact changed span.
const MINUS_BG: Color = Color::Rgb(74, 34, 34); // removed line (dark red)
const MINUS_EMPH_BG: Color = Color::Rgb(122, 48, 48); // removed word (brighter red)
const PLUS_BG: Color = Color::Rgb(30, 66, 41); // added line (dark green)
const PLUS_EMPH_BG: Color = Color::Rgb(47, 104, 61); // added word (brighter green)
const TEXT: Color = Color::Rgb(201, 209, 217); // #c9d1d9 default code foreground
const NUM: Color = Color::Rgb(110, 118, 129); // #6e7681 line numbers
const NUM_DIM: Color = Color::Rgb(72, 79, 88); // context line numbers
const HUNK_BG: Color = Color::Rgb(24, 32, 54); // subtle blue hunk header band
const HUNK_FG: Color = Color::Rgb(88, 166, 255); // #58a6ff hunk header text

fn num_str(n: Option<u32>) -> String {
    match n {
        Some(v) => format!("{v:>4}"),
        None => "    ".to_string(),
    }
}

/// Truncate-or-pad `s` to exactly `w` cells (char-count approximation).
fn fit(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    let len = out.chars().count();
    if len < w {
        out.push_str(&" ".repeat(w - len));
    }
    out
}

/// Longest-common-prefix/suffix "word" diff. Returns the emphasized middle
/// char-range for each of `a` and `b`, or `None` when the two lines share no
/// common affix at all (treat as wholly changed, no intra-line emphasis).
fn emph_ranges(a: &str, b: &str) -> (Option<(usize, usize)>, Option<(usize, usize)>) {
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let min = ac.len().min(bc.len());
    let mut p = 0;
    while p < min && ac[p] == bc[p] {
        p += 1;
    }
    let mut s = 0;
    while s < (ac.len() - p).min(bc.len() - p) && ac[ac.len() - 1 - s] == bc[bc.len() - 1 - s] {
        s += 1;
    }
    if p == 0 && s == 0 {
        return (None, None);
    }
    let ar = (p, ac.len() - s);
    let br = (p, bc.len() - s);
    (
        (ar.0 < ar.1).then_some(ar),
        (br.0 < br.1).then_some(br),
    )
}

/// Render `text` (honouring horizontal scroll) into exactly `width` cells,
/// filling with `base` background and switching to `emph_bg` over `emph`. Each
/// character's foreground comes from `fgs` (syntax colours), falling back to
/// `default_fg`. Spans break whenever the (fg, bg) pair changes.
#[allow(clippy::too_many_arguments)]
fn fill_body(
    text: &str,
    emph: Option<(usize, usize)>,
    base: Option<Color>,
    emph_bg: Color,
    fgs: &[Color],
    default_fg: Color,
    hscroll: usize,
    width: usize,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let base_bg = base.unwrap_or(Color::Reset);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur = String::new();
    let mut cur_key: Option<(Color, Color)> = None;
    let mut used = 0usize;

    for off in 0..width {
        let i = hscroll + off;
        if i >= chars.len() {
            break;
        }
        let is_emph = emph.map_or(false, |(a, b)| i >= a && i < b);
        let bg = if is_emph { emph_bg } else { base_bg };
        let fg = fgs.get(i).copied().unwrap_or(default_fg);
        let key = (fg, bg);
        if cur_key.is_some() && cur_key != Some(key) {
            let (f, b) = cur_key.unwrap();
            spans.push(Span::styled(
                std::mem::take(&mut cur),
                Style::default().fg(f).bg(b),
            ));
        }
        cur_key = Some(key);
        cur.push(chars[i]);
        used += 1;
    }
    if let Some((f, b)) = cur_key {
        if !cur.is_empty() {
            spans.push(Span::styled(cur, Style::default().fg(f).bg(b)));
        }
    }
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(base_bg),
        ));
    }
    spans
}

fn hunk_header_line(header: &str, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        fit(header, width),
        Style::default()
            .fg(HUNK_FG)
            .bg(HUNK_BG)
            .add_modifier(Modifier::BOLD),
    ))
}

fn message_lines(text: &str, color: Color) -> Vec<Line<'static>> {
    vec![Line::from(Span::styled(text.to_string(), Style::default().fg(color)))]
}

fn build_inline(
    fd: &FileDiff,
    width: u16,
    hscroll: usize,
    hl: &Highlighter,
    syntax: &SyntaxReference,
) -> Vec<Line<'static>> {
    if fd.binary {
        return message_lines("Binary file — no textual diff.", Color::Yellow);
    }
    if fd.is_empty() {
        return message_lines("No textual changes (metadata/mode only).", CTX);
    }

    let w = width as usize;
    let gutter_w = 10; // "NNNN NNNN " -> 4 + 1 + 4 + 1
    let text_w = w.saturating_sub(gutter_w).max(1);

    // One changed line: two-column gutter + full-width coloured body.
    let changed = |old, new, text: &str, other: Option<&str>, base, emph_bg| -> Line<'static> {
        let emph = other.and_then(|o| emph_ranges(text, o).0);
        let fgs = hl.line_colors(syntax, text);
        let mut spans = vec![Span::styled(
            format!("{} {} ", num_str(old), num_str(new)),
            Style::default().fg(NUM).bg(base),
        )];
        spans.extend(fill_body(
            text,
            emph,
            Some(base),
            emph_bg,
            &fgs,
            TEXT,
            hscroll,
            text_w,
        ));
        Line::from(spans)
    };

    let mut out = Vec::new();
    for hunk in &fd.hunks {
        out.push(hunk_header_line(&hunk.header, w));
        let lines = &hunk.lines;
        let mut i = 0;
        while i < lines.len() {
            if lines[i].kind == LineKind::Context {
                let l = &lines[i];
                let fgs = hl.line_colors(syntax, &l.text);
                let mut spans = vec![Span::styled(
                    format!("{} {} ", num_str(l.old_no), num_str(l.new_no)),
                    Style::default().fg(NUM_DIM),
                )];
                spans.extend(fill_body(
                    &l.text,
                    None,
                    None,
                    Color::Reset,
                    &fgs,
                    TEXT,
                    hscroll,
                    text_w,
                ));
                out.push(Line::from(spans));
                i += 1;
                continue;
            }
            // Gather a maximal run of changed lines, split removed vs added.
            let (mut rem, mut add): (Vec<&_>, Vec<&_>) = (Vec::new(), Vec::new());
            while i < lines.len() && lines[i].kind != LineKind::Context {
                match lines[i].kind {
                    LineKind::Removed => rem.push(&lines[i]),
                    LineKind::Added => add.push(&lines[i]),
                    LineKind::Context => {}
                }
                i += 1;
            }
            for (idx, l) in rem.iter().enumerate() {
                let other = add.get(idx).map(|o| o.text.as_str());
                out.push(changed(l.old_no, None, &l.text, other, MINUS_BG, MINUS_EMPH_BG));
            }
            for (idx, l) in add.iter().enumerate() {
                let other = rem.get(idx).map(|o| o.text.as_str());
                out.push(changed(None, l.new_no, &l.text, other, PLUS_BG, PLUS_EMPH_BG));
            }
        }
    }
    out
}

fn build_side(
    fd: &FileDiff,
    width: u16,
    hscroll: usize,
    hl: &Highlighter,
    syntax: &SyntaxReference,
) -> Vec<Line<'static>> {
    if fd.binary {
        return message_lines("Binary file — no textual diff.", Color::Yellow);
    }
    if fd.is_empty() {
        return message_lines("No textual changes (metadata/mode only).", CTX);
    }

    let w = width as usize;
    let side_w = w.saturating_sub(1) / 2;
    let num_w = 5; // 4-digit number + trailing space
    let text_w = side_w.saturating_sub(num_w).max(1);

    // One side of a row: number gutter + coloured body of exactly `side_w` cells.
    let cell = |numv: Option<u32>,
                text: &str,
                other: Option<&str>,
                base: Option<Color>,
                emph_bg: Color,
                default_fg: Color|
     -> Vec<Span<'static>> {
        let emph = other.and_then(|o| emph_ranges(text, o).0);
        let fgs = hl.line_colors(syntax, text);
        let base_bg = base.unwrap_or(Color::Reset);
        let num_fg = if base.is_some() { NUM } else { NUM_DIM };
        let mut spans = vec![Span::styled(
            format!("{} ", num_str(numv)),
            Style::default().fg(num_fg).bg(base_bg),
        )];
        spans.extend(fill_body(
            text, emph, base, emph_bg, &fgs, default_fg, hscroll, text_w,
        ));
        spans
    };
    let blank = || vec![Span::styled(" ".repeat(num_w + text_w), Style::default())];
    let sep = || Span::styled("│", Style::default().fg(NUM));

    let mut out = Vec::new();
    for row in diff::to_side_rows(fd) {
        match row {
            SideRow::Header(h) => out.push(hunk_header_line(&h, w)),
            SideRow::Context { old, new, text } => {
                let mut spans = cell(Some(old), &text, None, None, Color::Reset, TEXT);
                spans.push(sep());
                spans.extend(cell(Some(new), &text, None, None, Color::Reset, TEXT));
                out.push(Line::from(spans));
            }
            SideRow::Pair { left, right } => {
                let lt = left.as_ref().map(|(_, t)| t.as_str());
                let rt = right.as_ref().map(|(_, t)| t.as_str());
                let mut spans = match &left {
                    Some((n, t)) => cell(Some(*n), t, rt, Some(MINUS_BG), MINUS_EMPH_BG, TEXT),
                    None => blank(),
                };
                spans.push(sep());
                match &right {
                    Some((n, t)) => {
                        spans.extend(cell(Some(*n), t, lt, Some(PLUS_BG), PLUS_EMPH_BG, TEXT))
                    }
                    None => spans.extend(blank()),
                }
                out.push(Line::from(spans));
            }
        }
    }
    out
}
