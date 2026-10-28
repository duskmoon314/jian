use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use markdown::{Constructs, ParseOptions, mdast::Node, to_mdast};
use percent_encoding::percent_decode_str;

use crate::workspace::Workspace;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Backlink {
    pub source: PathBuf,
    pub line: usize,
    pub snippet: String,
}

/// Compute standard-Markdown backlinks on demand. No persistent index or
/// WikiLink support is involved.
pub fn find_backlinks(workspace: &Workspace, current: &Path) -> Result<Vec<Backlink>> {
    let current = fs::canonicalize(current)
        .with_context(|| format!("failed to resolve {}", current.display()))?;
    let mut backlinks = Vec::new();

    for relative in workspace.markdown_files() {
        let source_path = workspace.root().join(relative);
        let source = fs::read_to_string(&source_path)
            .with_context(|| format!("failed to read {}", source_path.display()))?;
        collect_document_backlinks(
            workspace.root(),
            relative,
            &source_path,
            &source,
            &current,
            &mut backlinks,
        )?;
    }

    backlinks.sort_by(|a, b| (&a.source, a.line).cmp(&(&b.source, b.line)));
    backlinks.dedup();
    Ok(backlinks)
}

fn collect_document_backlinks(
    root: &Path,
    relative: &Path,
    source_path: &Path,
    source: &str,
    current: &Path,
    output: &mut Vec<Backlink>,
) -> Result<()> {
    let options = ParseOptions {
        constructs: Constructs {
            frontmatter: true,
            ..Constructs::gfm()
        },
        ..ParseOptions::gfm()
    };
    let ast = to_mdast(source, &options).map_err(|error| anyhow!(error.to_string()))?;
    let mut definitions = HashMap::new();
    collect_definitions(&ast, &mut definitions);
    let mut record = |url: &str, position: Option<&markdown::unist::Position>| {
        let Some(destination) = resolve_link(root, source_path, url) else {
            return;
        };
        if destination != current {
            return;
        }
        let line = position.map_or(1, |position| position.start.line);
        let snippet = source
            .lines()
            .nth(line.saturating_sub(1))
            .unwrap_or_default()
            .trim()
            .to_owned();
        output.push(Backlink {
            source: relative.to_path_buf(),
            line,
            snippet,
        });
    };
    visit_links(&ast, &mut record);
    collect_reference_links(&ast, &definitions, &mut record);
    Ok(())
}

fn collect_definitions(node: &Node, definitions: &mut HashMap<String, String>) {
    if let Node::Definition(definition) = node {
        definitions.insert(definition.identifier.clone(), definition.url.clone());
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_definitions(child, definitions);
        }
    }
}

fn visit_links(node: &Node, visitor: &mut impl FnMut(&str, Option<&markdown::unist::Position>)) {
    match node {
        Node::Link(link) => visitor(&link.url, link.position.as_ref()),
        Node::LinkReference(link) => {
            // Definitions are resolved by the caller's second pass below.
            let _ = link;
        }
        _ => {}
    }
    if let Some(children) = node.children() {
        for child in children {
            visit_links(child, visitor);
        }
    }
}

fn collect_reference_links(
    node: &Node,
    definitions: &HashMap<String, String>,
    visitor: &mut impl FnMut(&str, Option<&markdown::unist::Position>),
) {
    if let Node::LinkReference(link) = node
        && let Some(url) = definitions.get(&link.identifier)
    {
        visitor(url, link.position.as_ref());
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_reference_links(child, definitions, visitor);
        }
    }
}

fn resolve_link(root: &Path, source_path: &Path, url: &str) -> Option<PathBuf> {
    if url.is_empty() || url.contains("://") || url.starts_with("mailto:") {
        return None;
    }
    let path = url.split(['#', '?']).next().unwrap_or_default();
    let path = percent_decode_str(path).decode_utf8().ok()?;
    if path.is_empty() {
        return None;
    }
    let decoded = path.as_ref();
    let candidate = if decoded.starts_with('/') {
        root.join(decoded.trim_start_matches('/'))
    } else {
        source_path.parent()?.join(decoded)
    };
    fs::canonicalize(candidate).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decodes_targets_and_ignores_frontmatter_text() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".jian")).unwrap();
        let current = temp.path().join("my note.md");
        fs::write(&current, "# Current\n").unwrap();
        fs::write(
            temp.path().join("source.md"),
            "---\nexample: '[fake](my%20note.md)'\n---\n\n[real](my%20note.md)\n",
        )
        .unwrap();

        let workspace = Workspace::discover(temp.path()).unwrap().unwrap();
        let links = find_backlinks(&workspace, &current).unwrap();
        assert_eq!(links.len(), 1);
        assert!(links[0].snippet.contains("real"));
    }

    #[test]
    fn finds_relative_and_reference_links_but_not_code() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".jian")).unwrap();
        fs::create_dir(temp.path().join("docs")).unwrap();
        let current = temp.path().join("docs/current.md");
        fs::write(&current, "# Current\n").unwrap();
        fs::write(
            temp.path().join("a.md"),
            "[one](docs/current.md)\n\n`[code](docs/current.md)`\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("docs/b.md"),
            "[two](current.md#section)\n\n[three][ref]\n\n[ref]: current.md\n",
        )
        .unwrap();

        let workspace = Workspace::discover(temp.path()).unwrap().unwrap();
        let links = find_backlinks(&workspace, &current).unwrap();
        assert_eq!(links.len(), 3, "{links:?}");
        assert!(links.iter().any(|link| link.snippet.contains("one")));
        assert!(links.iter().any(|link| link.snippet.contains("two")));
        assert!(!links.iter().any(|link| link.snippet.contains("code")));
    }
}
