//! Build a collapsible file tree from a flat list of changed paths.

use crate::git::ChangedFile;
use std::collections::HashSet;

pub enum NodeKind {
    Dir,
    File { index: usize, status: char },
}

pub struct Node {
    pub name: String,
    pub path: String,
    pub kind: NodeKind,
    pub children: Vec<Node>,
}

/// One row in the flattened, currently-visible tree.
pub struct VisibleRow {
    pub depth: usize,
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub file_index: Option<usize>,
    pub status: Option<char>,
}

fn insert(nodes: &mut Vec<Node>, parts: &[&str], prefix: &str, index: usize, status: char) {
    let name = parts[0];
    let full = if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    };
    if parts.len() == 1 {
        nodes.push(Node {
            name: name.to_string(),
            path: full,
            kind: NodeKind::File { index, status },
            children: Vec::new(),
        });
        return;
    }
    if let Some(existing) = nodes
        .iter_mut()
        .find(|n| n.name == name && matches!(n.kind, NodeKind::Dir))
    {
        insert(&mut existing.children, &parts[1..], &full, index, status);
    } else {
        let mut node = Node {
            name: name.to_string(),
            path: full.clone(),
            kind: NodeKind::Dir,
            children: Vec::new(),
        };
        insert(&mut node.children, &parts[1..], &full, index, status);
        nodes.push(node);
    }
}

fn sort_nodes(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| {
        let a_dir = matches!(a.kind, NodeKind::Dir);
        let b_dir = matches!(b.kind, NodeKind::Dir);
        b_dir.cmp(&a_dir).then_with(|| a.name.cmp(&b.name))
    });
    for n in nodes.iter_mut() {
        sort_nodes(&mut n.children);
    }
}

/// Build the tree, directories first then files, each alphabetical.
pub fn build_tree(files: &[ChangedFile]) -> Vec<Node> {
    let mut roots: Vec<Node> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let parts: Vec<&str> = f.path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        insert(&mut roots, &parts, "", i, f.status);
    }
    sort_nodes(&mut roots);
    roots
}

/// Collect the paths of every directory node (used to expand-all by default).
pub fn all_dir_paths(nodes: &[Node], out: &mut HashSet<String>) {
    for n in nodes {
        if matches!(n.kind, NodeKind::Dir) {
            out.insert(n.path.clone());
            all_dir_paths(&n.children, out);
        }
    }
}

/// Flatten the tree into visible rows honouring the `expanded` set.
pub fn flatten(
    nodes: &[Node],
    expanded: &HashSet<String>,
    depth: usize,
    out: &mut Vec<VisibleRow>,
) {
    for n in nodes {
        match &n.kind {
            NodeKind::Dir => {
                let is_exp = expanded.contains(&n.path);
                out.push(VisibleRow {
                    depth,
                    name: n.name.clone(),
                    path: n.path.clone(),
                    is_dir: true,
                    expanded: is_exp,
                    file_index: None,
                    status: None,
                });
                if is_exp {
                    flatten(&n.children, expanded, depth + 1, out);
                }
            }
            NodeKind::File { index, status } => {
                out.push(VisibleRow {
                    depth,
                    name: n.name.clone(),
                    path: n.path.clone(),
                    is_dir: false,
                    expanded: false,
                    file_index: Some(*index),
                    status: Some(*status),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cf(status: char, path: &str) -> ChangedFile {
        ChangedFile {
            status,
            path: path.to_string(),
            old_path: None,
            untracked: false,
        }
    }

    fn all_expanded(roots: &[Node]) -> HashSet<String> {
        let mut s = HashSet::new();
        all_dir_paths(roots, &mut s);
        s
    }

    #[test]
    fn nests_paths_into_directories() {
        let files = vec![cf('M', "src/app.rs"), cf('A', "src/ui/mod.rs")];
        let roots = build_tree(&files);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "src");
        assert!(matches!(roots[0].kind, NodeKind::Dir));
        assert_eq!(roots[0].path, "src");
        // src contains the `ui` dir and the `app.rs` file.
        let names: Vec<&str> = roots[0].children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["ui", "app.rs"]); // dirs before files
    }

    #[test]
    fn directories_sort_before_files_then_alphabetical() {
        let files = vec![
            cf('M', "z.txt"),
            cf('M', "a.txt"),
            cf('M', "beta/x.rs"),
            cf('M', "alpha/y.rs"),
        ];
        let roots = build_tree(&files);
        let names: Vec<&str> = roots.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "a.txt", "z.txt"]);
    }

    #[test]
    fn file_index_points_back_to_source_slice() {
        let files = vec![cf('A', "a.rs"), cf('D', "dir/b.rs")];
        let roots = build_tree(&files);
        let expanded = all_expanded(&roots);
        let mut rows = Vec::new();
        flatten(&roots, &expanded, 0, &mut rows);

        for row in &rows {
            if let Some(i) = row.file_index {
                assert_eq!(row.path, files[i].path);
                assert_eq!(row.status, Some(files[i].status));
            }
        }
    }

    #[test]
    fn all_dir_paths_collects_nested_dirs() {
        let files = vec![cf('M', "a/b/c.rs"), cf('M', "a/d.rs")];
        let roots = build_tree(&files);
        let dirs = all_expanded(&roots);
        assert!(dirs.contains("a"));
        assert!(dirs.contains("a/b"));
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn collapsed_dir_hides_its_children() {
        let files = vec![cf('M', "a/b/c.rs"), cf('M', "top.rs")];
        let roots = build_tree(&files);

        // Fully expanded: dir a, dir a/b, file c.rs, file top.rs = 4 rows.
        let expanded = all_expanded(&roots);
        let mut rows = Vec::new();
        flatten(&roots, &expanded, 0, &mut rows);
        assert_eq!(rows.len(), 4);

        // Collapse everything: only the top-level dir `a` and file `top.rs`.
        let none = HashSet::new();
        let mut rows = Vec::new();
        flatten(&roots, &none, 0, &mut rows);
        let visible: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(visible, vec!["a", "top.rs"]);
        assert!(!rows[0].expanded);
    }

    #[test]
    fn flatten_reports_depth() {
        let files = vec![cf('M', "a/b/c.rs")];
        let roots = build_tree(&files);
        let expanded = all_expanded(&roots);
        let mut rows = Vec::new();
        flatten(&roots, &expanded, 0, &mut rows);
        let depths: Vec<usize> = rows.iter().map(|r| r.depth).collect();
        assert_eq!(depths, vec![0, 1, 2]); // a, a/b, c.rs
    }
}
