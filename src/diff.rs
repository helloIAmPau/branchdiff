//! Parsing of git's unified-diff text into a structured model, plus a transform
//! into side-by-side rows.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

pub struct FileDiff {
    pub hunks: Vec<Hunk>,
    pub binary: bool,
}

impl FileDiff {
    pub fn is_empty(&self) -> bool {
        !self.binary && self.hunks.iter().all(|h| h.lines.is_empty())
    }
}

/// Parse the `@@ -a,b +c,d @@` header, returning the starting old/new line nums.
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let end = rest.find(" @@")?;
    let ranges = &rest[..end];
    let mut parts = ranges.split(' ');
    let old = parts.next()?; // like -1,4  or -1
    let new = parts.next()?; // like +1,6  or +1
    let old_start = old.trim_start_matches('-').split(',').next()?.parse().ok()?;
    let new_start = new.trim_start_matches('+').split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

/// Turn raw unified-diff text into a [`FileDiff`].
pub fn parse_diff(raw: &str) -> FileDiff {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut binary = false;
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut in_hunk = false;

    for line in raw.split('\n') {
        if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            binary = true;
            continue;
        }
        if line.starts_with("@@") {
            if let Some((os, ns)) = parse_hunk_header(line) {
                old_no = os;
                new_no = ns;
            }
            hunks.push(Hunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
            in_hunk = true;
            continue;
        }
        // Everything before the first @@ (diff --git, index, ---, +++) is skipped.
        if !in_hunk {
            continue;
        }
        let hunk = match hunks.last_mut() {
            Some(h) => h,
            None => continue,
        };
        // The prefix markers are single ASCII bytes, so byte-slicing `line[1..]` is safe.
        match line.as_bytes().first() {
            Some(b'+') => {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Added,
                    old_no: None,
                    new_no: Some(new_no),
                    text: normalize(&line[1..]),
                });
                new_no += 1;
            }
            Some(b'-') => {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Removed,
                    old_no: Some(old_no),
                    new_no: None,
                    text: normalize(&line[1..]),
                });
                old_no += 1;
            }
            Some(b' ') => {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Context,
                    old_no: Some(old_no),
                    new_no: Some(new_no),
                    text: normalize(&line[1..]),
                });
                old_no += 1;
                new_no += 1;
            }
            // "\ No newline at end of file" and stray blank lines are ignored.
            _ => {}
        }
    }

    FileDiff { hunks, binary }
}

/// Expand tabs so column alignment survives in the TUI.
fn normalize(s: &str) -> String {
    s.replace('\t', "    ")
}

// ---- side-by-side view -----------------------------------------------------

pub enum SideRow {
    Header(String),
    Context {
        old: u32,
        new: u32,
        text: String,
    },
    Pair {
        left: Option<(u32, String)>,
        right: Option<(u32, String)>,
    },
}

fn flush_pairs(
    rows: &mut Vec<SideRow>,
    rem: &mut Vec<(u32, String)>,
    add: &mut Vec<(u32, String)>,
) {
    let n = rem.len().max(add.len());
    for i in 0..n {
        rows.push(SideRow::Pair {
            left: rem.get(i).cloned(),
            right: add.get(i).cloned(),
        });
    }
    rem.clear();
    add.clear();
}

/// Convert a [`FileDiff`] into aligned left/right rows. Consecutive removed and
/// added lines within a hunk are paired positionally; context lines span both
/// sides.
pub fn to_side_rows(fd: &FileDiff) -> Vec<SideRow> {
    let mut rows = Vec::new();
    for hunk in &fd.hunks {
        rows.push(SideRow::Header(hunk.header.clone()));
        let mut rem: Vec<(u32, String)> = Vec::new();
        let mut add: Vec<(u32, String)> = Vec::new();
        for l in &hunk.lines {
            match l.kind {
                LineKind::Removed => rem.push((l.old_no.unwrap_or(0), l.text.clone())),
                LineKind::Added => add.push((l.new_no.unwrap_or(0), l.text.clone())),
                LineKind::Context => {
                    flush_pairs(&mut rows, &mut rem, &mut add);
                    rows.push(SideRow::Context {
                        old: l.old_no.unwrap_or(0),
                        new: l.new_no.unwrap_or(0),
                        text: l.text.clone(),
                    });
                }
            }
        }
        flush_pairs(&mut rows, &mut rem, &mut add);
    }
    rows
}
