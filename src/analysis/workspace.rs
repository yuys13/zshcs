use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::{Position, Url};

use crate::analysis::model::DocumentIndex;
use crate::analysis::parser::parse_document;
use crate::document::DocumentManager;

pub const MAX_SOURCE_DEPTH: usize = 4;
pub const MAX_SOURCED_FILES: usize = 64;
const MAX_SOURCED_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB

/// A resolved and parsed sourced script document.
#[derive(Debug, Clone)]
pub struct SourcedDocument {
    pub uri: Url,
    pub text: String,
    pub index: DocumentIndex,
}

#[inline]
fn pos_le(a: Position, b: Position) -> bool {
    a.line < b.line || (a.line == b.line && a.character <= b.character)
}

pub fn normalize_lexical_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut stack: Vec<Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(stack.last(), Some(Component::Normal(_))) {
                    stack.pop();
                } else if !matches!(
                    stack.last(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    stack.push(comp);
                }
            }
            _ => stack.push(comp),
        }
    }
    stack.iter().collect()
}

pub fn read_sourced_file_bounded(path: &Path) -> Option<String> {
    use std::io::Read;

    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCED_FILE_BYTES {
        return None;
    }

    let file = std::fs::File::open(path).ok()?;
    let opened_metadata = file.metadata().ok()?;
    if !opened_metadata.is_file() || opened_metadata.len() > MAX_SOURCED_FILE_BYTES {
        return None;
    }

    let mut buf = Vec::new();
    file.take(MAX_SOURCED_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() as u64 > MAX_SOURCED_FILE_BYTES {
        return None;
    }

    String::from_utf8(buf).ok()
}

pub fn expand_and_normalize_source_path(path_str: &str, base_uri: &Url) -> Option<PathBuf> {
    if path_str.is_empty() {
        return None;
    }

    let mut expanded_str = path_str.to_string();
    if let Ok(home) = std::env::var("HOME") {
        expanded_str = expanded_str
            .replace("$HOME", &home)
            .replace("${HOME}", &home);
        if expanded_str == "~" {
            expanded_str = home;
        } else if let Some(sub) = expanded_str.strip_prefix("~/") {
            expanded_str = PathBuf::from(home).join(sub).to_string_lossy().into_owned();
        }
    }

    let p = Path::new(&expanded_str);
    if p.is_absolute() {
        Some(normalize_lexical_path(p))
    } else {
        let base_path = base_uri.to_file_path().ok()?;
        let parent = base_path.parent()?;
        Some(normalize_lexical_path(&parent.join(p)))
    }
}

/// Resolves a source path relative to the current document's directory or as an absolute / tilde path.
pub fn resolve_source_path(path_str: &str, base_uri: &Url) -> Option<Url> {
    if path_str.is_empty() {
        return None;
    }

    let mut expanded_str = path_str.to_string();
    if let Ok(home) = std::env::var("HOME") {
        expanded_str = expanded_str
            .replace("$HOME", &home)
            .replace("${HOME}", &home);
    }

    // Tilde expansion (~ or ~/...)
    if (expanded_str == "~" || expanded_str.starts_with("~/"))
        && let Ok(home) = std::env::var("HOME")
    {
        let sub = if expanded_str == "~" {
            ""
        } else {
            &expanded_str[2..]
        };
        let candidate = PathBuf::from(home).join(sub);
        if candidate.exists() {
            if let Ok(canon) = candidate.canonicalize() {
                return Url::from_file_path(canon).ok();
            }
            return Url::from_file_path(candidate).ok();
        }
    }

    let path_obj = Path::new(&expanded_str);
    if path_obj.is_absolute() {
        if path_obj.exists() {
            if let Ok(canon) = path_obj.canonicalize() {
                return Url::from_file_path(canon).ok();
            }
            return Url::from_file_path(path_obj).ok();
        }
        return None;
    }

    // Relative path resolution
    if let Ok(base_path) = base_uri.to_file_path()
        && let Some(parent) = base_path.parent()
    {
        let candidate = parent.join(&expanded_str);
        if candidate.exists() {
            if let Ok(canon) = candidate.canonicalize() {
                return Url::from_file_path(canon).ok();
            }
            return Url::from_file_path(candidate).ok();
        }
    }

    None
}

pub fn resolve_open_sourced_uri(
    path_str: &str,
    base_uri: &Url,
    doc_manager: Option<&DocumentManager>,
) -> Option<Url> {
    let raw_path = expand_and_normalize_source_path(path_str, base_uri)?;
    let raw_uri = Url::from_file_path(&raw_path).ok()?;
    if doc_manager?.get(&raw_uri).is_some() {
        Some(raw_uri)
    } else {
        None
    }
}

pub fn resolve_sourced_script(
    path_str: &str,
    base_uri: &Url,
    doc_manager: Option<&DocumentManager>,
) -> Option<(Url, Option<Url>, String)> {
    let raw_path = expand_and_normalize_source_path(path_str, base_uri)?;
    let raw_uri = Url::from_file_path(&raw_path).ok();
    let canon_uri = resolve_source_path(path_str, base_uri);

    let content = doc_manager
        .and_then(|mgr| {
            canon_uri
                .as_ref()
                .and_then(|u| mgr.get_content(u))
                .or_else(|| raw_uri.as_ref().and_then(|u| mgr.get_content(u)))
        })
        .or_else(|| {
            if let Some(ref u) = canon_uri
                && let Ok(p) = u.to_file_path()
            {
                return read_sourced_file_bounded(&p);
            }
            read_sourced_file_bounded(&raw_path)
        })?;

    let primary_uri = canon_uri.or_else(|| raw_uri.clone())?;
    Some((primary_uri, raw_uri, content))
}

fn prioritized_source_paths(
    index: &DocumentIndex,
    cursor_pos: Position,
    include_after_cursor: bool,
) -> Vec<String> {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for src in &index.sources {
        if pos_le(src.arg_end, cursor_pos) {
            before.push(src.path.clone());
        } else if include_after_cursor {
            after.push(src.path.clone());
        }
    }

    let mut seen = HashSet::with_capacity(before.len() + after.len());
    let mut out = Vec::with_capacity(before.len() + after.len());
    for p in before.into_iter().chain(after) {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

struct WorkspaceCollector<'a> {
    doc_manager: Option<&'a DocumentManager>,
    visited_uris: HashSet<Url>,
    docs: Vec<SourcedDocument>,
}

impl<'a> WorkspaceCollector<'a> {
    fn collect(
        &mut self,
        index: &DocumentIndex,
        base_uri: &Url,
        cursor_pos: Position,
        include_after_cursor: bool,
        depth: usize,
    ) {
        if depth >= MAX_SOURCE_DEPTH || self.docs.len() >= MAX_SOURCED_FILES {
            return;
        }

        for path_str in prioritized_source_paths(index, cursor_pos, include_after_cursor) {
            if self.docs.len() >= MAX_SOURCED_FILES {
                break;
            }

            let Some((target_uri, alt_uri, content)) =
                resolve_sourced_script(&path_str, base_uri, self.doc_manager)
            else {
                continue;
            };

            let already_visited = self.visited_uris.contains(&target_uri)
                || alt_uri
                    .as_ref()
                    .is_some_and(|u| self.visited_uris.contains(u));
            if already_visited {
                continue;
            }

            self.visited_uris.insert(target_uri.clone());
            if let Some(alt) = alt_uri {
                self.visited_uris.insert(alt);
            }

            let child_index = parse_document(&content);
            self.docs.push(SourcedDocument {
                uri: target_uri.clone(),
                text: content,
                index: child_index,
            });

            // Recurse into nested sourced scripts using a cloned reference via index
            let child_idx_clone = self.docs.last().unwrap().index.clone();
            self.collect(
                &child_idx_clone,
                &target_uri,
                Position::new(u32::MAX, u32::MAX),
                true,
                depth + 1,
            );
        }
    }
}

/// Collects and parses sourced scripts referenced from `index` in priority order,
/// with cycle detection, depth cap (`MAX_SOURCE_DEPTH`), and file count cap (`MAX_SOURCED_FILES`).
pub fn collect_sourced_documents(
    index: &DocumentIndex,
    base_uri: &Url,
    cursor_pos: Position,
    doc_manager: Option<&DocumentManager>,
    include_after_cursor: bool,
) -> Vec<SourcedDocument> {
    let mut visited_uris = HashSet::new();
    visited_uris.insert(base_uri.clone());
    if let Ok(path) = base_uri.to_file_path()
        && let Ok(canon) = path.canonicalize()
        && let Ok(canon_uri) = Url::from_file_path(canon)
    {
        visited_uris.insert(canon_uri);
    }

    let mut collector = WorkspaceCollector {
        doc_manager,
        visited_uris,
        docs: Vec::new(),
    };
    collector.collect(index, base_uri, cursor_pos, include_after_cursor, 0);
    collector.docs
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_collect_sourced_documents_cycle_and_in_memory_priority() {
        let dir = tempdir().unwrap();
        let main_path = dir.path().join("main.zsh");
        let a_path = dir.path().join("a.zsh");
        let b_path = dir.path().join("b.zsh");

        std::fs::write(&a_path, "source ./b.zsh\nfn_a() { : }\n").unwrap();
        std::fs::write(&b_path, "source ./a.zsh\nVAR_B=1\n").unwrap();

        let main_uri = Url::from_file_path(&main_path).unwrap();
        let a_uri = Url::from_file_path(&a_path).unwrap();
        let doc_mgr = DocumentManager::new();
        doc_mgr.open(
            a_uri.clone(),
            1,
            "source ./b.zsh\nfn_a_in_memory() { : }\n".to_string(),
        );

        let main_text = "source ./a.zsh\necho hi\n";
        let main_idx = parse_document(main_text);
        let sourced = collect_sourced_documents(
            &main_idx,
            &main_uri,
            Position::new(1, 0),
            Some(&doc_mgr),
            false,
        );

        assert_eq!(sourced.len(), 2);
        assert_eq!(sourced[0].index.functions[0].name, "fn_a_in_memory");
        assert_eq!(sourced[1].index.variables[0].name, "VAR_B");
    }
}
