//! Parsing of git's unified-diff text into a structured model, plus a transform
//! into side-by-side rows.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic unified diff: one modified line surrounded by context.
    const SAMPLE: &str = "\
diff --git a/foo.txt b/foo.txt
index 111..222 100644
--- a/foo.txt
+++ b/foo.txt
@@ -1,4 +1,4 @@
 alpha
-beta
+BETA
 gamma
 delta";

    #[test]
    fn parses_header_ranges() {
        // Returns the *start* line on each side, not the counts.
        assert_eq!(parse_hunk_header("@@ -1,4 +1,4 @@"), Some((1, 1)));
        assert_eq!(parse_hunk_header("@@ -3,4 +7,4 @@"), Some((3, 7)));
        assert_eq!(parse_hunk_header("@@ -10 +12,3 @@"), Some((10, 12)));
        // Trailing section headers (git's function context) are tolerated.
        assert_eq!(parse_hunk_header("@@ -5,2 +6,2 @@ fn main()"), Some((5, 6)));
        assert_eq!(parse_hunk_header("not a hunk"), None);
    }

    #[test]
    fn preamble_before_first_hunk_is_skipped() {
        let fd = parse_diff(SAMPLE);
        assert!(!fd.binary);
        assert_eq!(fd.hunks.len(), 1);
        // diff --git / index / --- / +++ lines must not become diff lines.
        assert_eq!(fd.hunks[0].lines.len(), 5);
    }

    #[test]
    fn classifies_and_numbers_lines() {
        let fd = parse_diff(SAMPLE);
        let lines = &fd.hunks[0].lines;

        assert_eq!(lines[0].kind, LineKind::Context);
        assert_eq!(lines[0].old_no, Some(1));
        assert_eq!(lines[0].new_no, Some(1));
        assert_eq!(lines[0].text, "alpha");

        assert_eq!(lines[1].kind, LineKind::Removed);
        assert_eq!(lines[1].old_no, Some(2));
        assert_eq!(lines[1].new_no, None);
        assert_eq!(lines[1].text, "beta");

        assert_eq!(lines[2].kind, LineKind::Added);
        assert_eq!(lines[2].old_no, None);
        assert_eq!(lines[2].new_no, Some(2));
        assert_eq!(lines[2].text, "BETA");

        // Context after the change keeps counting on both sides.
        assert_eq!(lines[3].kind, LineKind::Context);
        assert_eq!(lines[3].old_no, Some(3));
        assert_eq!(lines[3].new_no, Some(3));
    }

    #[test]
    fn numbers_advance_across_multiple_hunks() {
        let raw = "\
@@ -1,2 +1,2 @@
 a
+b
@@ -10,1 +11,2 @@
-x
+y
+z";
        let fd = parse_diff(raw);
        assert_eq!(fd.hunks.len(), 2);
        let h2 = &fd.hunks[1].lines;
        assert_eq!(h2[0].kind, LineKind::Removed);
        assert_eq!(h2[0].old_no, Some(10));
        assert_eq!(h2[1].new_no, Some(11));
        assert_eq!(h2[2].new_no, Some(12));
    }

    #[test]
    fn tabs_are_expanded() {
        let fd = parse_diff("@@ -1 +1 @@\n+\tindented");
        assert_eq!(fd.hunks[0].lines[0].text, "    indented");
    }

    #[test]
    fn no_newline_marker_is_ignored() {
        let fd = parse_diff("@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file");
        assert_eq!(fd.hunks[0].lines.len(), 2);
    }

    #[test]
    fn detects_binary_files() {
        let fd = parse_diff("diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ");
        assert!(fd.binary);
        assert!(!fd.is_empty()); // binary counts as content
    }

    #[test]
    fn is_empty_only_when_no_hunks_and_not_binary() {
        assert!(parse_diff("").is_empty());
        assert!(!parse_diff(SAMPLE).is_empty());
    }

    #[test]
    fn side_rows_pair_removed_with_added() {
        let fd = parse_diff(SAMPLE);
        let rows = to_side_rows(&fd);
        // Header, context(alpha), pair(beta/BETA), context(gamma), context(delta)
        assert!(matches!(rows[0], SideRow::Header(_)));
        assert!(matches!(rows[1], SideRow::Context { .. }));
        match &rows[2] {
            SideRow::Pair { left, right } => {
                assert_eq!(left.as_ref().unwrap().1, "beta");
                assert_eq!(right.as_ref().unwrap().1, "BETA");
            }
            _ => panic!("expected a paired row"),
        }
    }

    #[test]
    fn side_rows_handle_uneven_add_remove() {
        // One removed, two added -> two pairs, second has no left side.
        let fd = parse_diff("@@ -1,1 +1,2 @@\n-only\n+first\n+second");
        let rows = to_side_rows(&fd);
        let pairs: Vec<_> = rows
            .iter()
            .filter(|r| matches!(r, SideRow::Pair { .. }))
            .collect();
        assert_eq!(pairs.len(), 2);
        match pairs[1] {
            SideRow::Pair { left, right } => {
                assert!(left.is_none());
                assert_eq!(right.as_ref().unwrap().1, "second");
            }
            _ => unreachable!(),
        }
    }
}
