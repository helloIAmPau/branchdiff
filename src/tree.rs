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
