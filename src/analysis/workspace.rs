use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower_lsp::lsp_types::{Position, Url};

use crate::analysis::model::DocumentIndex;
use crate::analysis::parser::parse_document;
use crate::document::DocumentManager;

pub const MAX_SOURCE_DEPTH: usize = 4;
pub const MAX_SOURCED_FILES: usize = 64;
pub const MAX_SOURCED_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB

/// A resolved and parsed sourced script document.
#[derive(Debug, Clone)]
pub struct SourcedDocument {
    pub uri: Url,
    pub text: String,
    pub index: Arc<DocumentIndex>,
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
    let (primary_uri, raw_uri, content, _) =
        resolve_sourced_script_with_index(path_str, base_uri, doc_manager)?;
    Some((primary_uri, raw_uri, content))
}

struct ResolvedSourceUris {
    raw_path: PathBuf,
    raw_uri: Option<Url>,
    canon_uri: Option<Url>,
    primary_uri: Url,
}

fn resolve_source_uris(path_str: &str, base_uri: &Url) -> Option<ResolvedSourceUris> {
    let raw_path = expand_and_normalize_source_path(path_str, base_uri)?;
    let raw_uri = Url::from_file_path(&raw_path).ok();
    let canon_uri = resolve_source_path(path_str, base_uri);
    let primary_uri = canon_uri.clone().or_else(|| raw_uri.clone())?;
    Some(ResolvedSourceUris {
        raw_path,
        raw_uri,
        canon_uri,
        primary_uri,
    })
}

fn load_sourced_script_with_index(
    uris: &ResolvedSourceUris,
    doc_manager: Option<&DocumentManager>,
) -> Option<(String, Arc<DocumentIndex>)> {
    #[cfg(test)]
    tests::increment_load_count();

    if let Some(mgr) = doc_manager {
        let open_doc = uris
            .canon_uri
            .as_ref()
            .and_then(|u| mgr.get(u))
            .or_else(|| uris.raw_uri.as_ref().and_then(|u| mgr.get(u)))
            .map(|doc| doc.clone());
        if let Some(doc) = open_doc {
            return Some((doc.text().to_string(), doc.index()));
        }
    }

    let content = if let Some(ref u) = uris.canon_uri
        && let Ok(p) = u.to_file_path()
    {
        read_sourced_file_bounded(&p)
    } else {
        read_sourced_file_bounded(&uris.raw_path)
    }?;

    let index = Arc::new(parse_document(&content));
    Some((content, index))
}

fn resolve_sourced_script_with_index(
    path_str: &str,
    base_uri: &Url,
    doc_manager: Option<&DocumentManager>,
) -> Option<(Url, Option<Url>, String, Arc<DocumentIndex>)> {
    let uris = resolve_source_uris(path_str, base_uri)?;
    let (content, index) = load_sourced_script_with_index(&uris, doc_manager)?;
    Some((uris.primary_uri, uris.raw_uri, content, index))
}

pub(crate) fn prioritized_source_paths(
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
    loaded_files: usize,
}

impl<'a> WorkspaceCollector<'a> {
    fn new(base_uri: &Url, doc_manager: Option<&'a DocumentManager>) -> Self {
        let mut visited_uris = HashSet::new();
        visited_uris.insert(base_uri.clone());
        if let Ok(path) = base_uri.to_file_path()
            && let Ok(canon) = path.canonicalize()
            && let Ok(canon_uri) = Url::from_file_path(canon)
        {
            visited_uris.insert(canon_uri);
        }

        Self {
            doc_manager,
            visited_uris,
            loaded_files: 0,
        }
    }

    fn visit<T, F>(
        &mut self,
        index: &DocumentIndex,
        base_uri: &Url,
        cursor_pos: Position,
        include_after_cursor: bool,
        depth: usize,
        visitor: &mut F,
    ) -> Option<T>
    where
        F: FnMut(&SourcedDocument) -> Option<T>,
    {
        if depth >= MAX_SOURCE_DEPTH || self.loaded_files >= MAX_SOURCED_FILES {
            return None;
        }

        for path_str in prioritized_source_paths(index, cursor_pos, include_after_cursor) {
            if self.loaded_files >= MAX_SOURCED_FILES {
                break;
            }

            let Some(uris) = resolve_source_uris(&path_str, base_uri) else {
                continue;
            };

            let already_visited = self.visited_uris.contains(&uris.primary_uri)
                || uris
                    .raw_uri
                    .as_ref()
                    .is_some_and(|u| self.visited_uris.contains(u));
            if already_visited {
                continue;
            }

            let Some((content, child_index)) =
                load_sourced_script_with_index(&uris, self.doc_manager)
            else {
                continue;
            };

            self.visited_uris.insert(uris.primary_uri.clone());
            if let Some(raw_uri) = uris.raw_uri {
                self.visited_uris.insert(raw_uri);
            }
            self.loaded_files += 1;

            let doc = SourcedDocument {
                uri: uris.primary_uri,
                text: content,
                index: child_index,
            };

            if let Some(found) = visitor(&doc) {
                return Some(found);
            }

            if let Some(found) = self.visit(
                &doc.index,
                &doc.uri,
                Position::new(u32::MAX, u32::MAX),
                true,
                depth + 1,
                visitor,
            ) {
                return Some(found);
            }
        }

        None
    }
}

/// Traverses sourced scripts referenced from `index` in priority order, invoking `visitor`
/// on each resolved document and short-circuiting as soon as `visitor` returns `Some(T)`.
/// Preserves cycle detection, depth cap (`MAX_SOURCE_DEPTH`), and file count cap (`MAX_SOURCED_FILES`).
pub fn find_in_sourced_documents<T, F>(
    index: &DocumentIndex,
    base_uri: &Url,
    cursor_pos: Position,
    doc_manager: Option<&DocumentManager>,
    include_after_cursor: bool,
    mut visitor: F,
) -> Option<T>
where
    F: FnMut(&SourcedDocument) -> Option<T>,
{
    let mut collector = WorkspaceCollector::new(base_uri, doc_manager);
    collector.visit(
        index,
        base_uri,
        cursor_pos,
        include_after_cursor,
        0,
        &mut visitor,
    )
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
    let mut docs = Vec::new();
    find_in_sourced_documents::<(), _>(
        index,
        base_uri,
        cursor_pos,
        doc_manager,
        include_after_cursor,
        |doc| {
            docs.push(doc.clone());
            None
        },
    );
    docs
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;
    use tempfile::tempdir;

    thread_local! {
        static SOURCED_LOAD_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn increment_load_count() {
        SOURCED_LOAD_COUNT.with(|c| c.set(c.get() + 1));
    }

    pub(crate) fn reset_load_count() {
        SOURCED_LOAD_COUNT.with(|c| c.set(0));
    }

    pub(crate) fn get_load_count() -> usize {
        SOURCED_LOAD_COUNT.with(|c| c.get())
    }

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

        // Open buffer should reuse the exact same cached Arc<DocumentIndex>
        let cached_a = doc_mgr.get_index(&a_uri).unwrap();
        assert!(Arc::ptr_eq(&sourced[0].index, &cached_a));
    }

    #[test]
    fn test_collect_sourced_documents_diamond_dependency_loads_common_once() {
        let dir = tempdir().unwrap();
        let sub_dir = dir.path().join("sub");
        std::fs::create_dir(&sub_dir).unwrap();

        let main_path = dir.path().join("main.zsh");
        let a_path = dir.path().join("a.zsh");
        let b_path = dir.path().join("b.zsh");
        let common_path = dir.path().join("common.zsh");

        std::fs::write(&common_path, "common_fn() { : }\nCOMMON_VAR=1\n").unwrap();
        std::fs::write(&a_path, "source ./common.zsh\na_fn() { : }\n").unwrap();
        // Relative path alias via sub/../common.zsh
        std::fs::write(&b_path, "source ./sub/../common.zsh\nb_fn() { : }\n").unwrap();

        let main_uri = Url::from_file_path(&main_path).unwrap();
        let main_text = "source ./a.zsh\nsource ./b.zsh\necho hi\n";
        let main_idx = parse_document(main_text);

        reset_load_count();
        let sourced =
            collect_sourced_documents(&main_idx, &main_uri, Position::new(2, 0), None, false);

        assert_eq!(sourced.len(), 3);
        let common_canon_uri = Url::from_file_path(common_path.canonicalize().unwrap()).unwrap();
        let common_count = sourced.iter().filter(|d| d.uri == common_canon_uri).count();
        assert_eq!(common_count, 1);
        // Must only load a.zsh, common.zsh, and b.zsh once each (3 loads total)
        assert_eq!(get_load_count(), 3);
    }

    #[test]
    fn test_collect_sourced_documents_cycle_skips_reloading_visited_files() {
        let dir = tempdir().unwrap();
        let main_path = dir.path().join("main.zsh");
        let a_path = dir.path().join("a.zsh");
        let b_path = dir.path().join("b.zsh");

        std::fs::write(&main_path, "source ./a.zsh\n").unwrap();
        std::fs::write(&a_path, "source ./b.zsh\nfn_a() { : }\n").unwrap();
        std::fs::write(&b_path, "source ./a.zsh\nsource ./main.zsh\nfn_b() { : }\n").unwrap();

        let main_uri = Url::from_file_path(&main_path).unwrap();
        let main_idx = parse_document("source ./a.zsh\n");

        reset_load_count();
        let sourced =
            collect_sourced_documents(&main_idx, &main_uri, Position::new(1, 0), None, true);

        assert_eq!(sourced.len(), 2);
        // Must only load a.zsh and b.zsh once each; back-edges to a.zsh and main.zsh must not load
        assert_eq!(get_load_count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn test_collect_sourced_documents_symlink_alias_loads_once() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let main_path = dir.path().join("main.zsh");
        let target_path = dir.path().join("real.zsh");
        let link_path = dir.path().join("alias.zsh");

        std::fs::write(&target_path, "real_fn() { : }\n").unwrap();
        symlink(&target_path, &link_path).unwrap();

        let main_uri = Url::from_file_path(&main_path).unwrap();
        let main_text = "source ./real.zsh\nsource ./alias.zsh\n";
        let main_idx = parse_document(main_text);

        reset_load_count();
        let sourced =
            collect_sourced_documents(&main_idx, &main_uri, Position::new(2, 0), None, true);

        assert_eq!(sourced.len(), 1);
        assert_eq!(get_load_count(), 1);
    }

    #[test]
    fn test_collect_sourced_documents_releases_dashmap_lock_before_parsing_open_doc() {
        let dir = tempdir().unwrap();
        let main_path = dir.path().join("main.zsh");
        let a_path = dir.path().join("a.zsh");

        std::fs::write(&a_path, "fn_disk() { : }\n").unwrap();

        let main_uri = Url::from_file_path(&main_path).unwrap();
        let a_uri = Url::from_file_path(&a_path).unwrap();
        let doc_mgr = DocumentManager::new();
        doc_mgr.open(a_uri.clone(), 1, "fn_in_memory() { : }\n".to_string());

        let mgr_clone = doc_mgr.clone();
        let a_uri_clone = a_uri.clone();
        crate::document::set_on_index_parse_hook(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            let handle = std::thread::spawn(move || {
                let acquired = mgr_clone.get_mut(&a_uri_clone).is_some();
                let _ = tx.send(acquired);
            });
            let acquired = rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .expect("DashMap shard write lock should not be blocked during sourced doc parse");
            assert!(acquired);
            handle.join().unwrap();
        });

        let main_idx = parse_document("source ./a.zsh\n");
        let sourced = collect_sourced_documents(
            &main_idx,
            &main_uri,
            Position::new(1, 0),
            Some(&doc_mgr),
            true,
        );
        assert_eq!(sourced.len(), 1);
        assert_eq!(sourced[0].index.functions[0].name, "fn_in_memory");
    }

    #[test]
    fn test_find_in_sourced_documents_short_circuits_without_loading_remaining_or_nested_files() {
        let dir = tempdir().unwrap();
        let main_path = dir.path().join("main.zsh");
        let a_path = dir.path().join("a.zsh");
        let b_path = dir.path().join("b.zsh");
        let nested_path = dir.path().join("nested.zsh");

        std::fs::write(&nested_path, "nested_fn() { : }\n").unwrap();
        std::fs::write(&a_path, "source ./nested.zsh\ntarget_fn() { : }\n").unwrap();
        std::fs::write(&b_path, "other_fn() { : }\n").unwrap();

        let main_uri = Url::from_file_path(&main_path).unwrap();
        let main_idx = parse_document("source ./a.zsh\nsource ./b.zsh\n");

        reset_load_count();
        let found = find_in_sourced_documents(
            &main_idx,
            &main_uri,
            Position::new(2, 0),
            None,
            true,
            |doc| {
                doc.index
                    .functions
                    .iter()
                    .find(|f| f.name == "target_fn")
                    .map(|f| (doc.uri.clone(), f.name.clone()))
            },
        );

        let a_canon_uri = Url::from_file_path(a_path.canonicalize().unwrap()).unwrap();
        assert_eq!(found, Some((a_canon_uri, "target_fn".to_string())));
        assert_eq!(
            get_load_count(),
            1,
            "Short-circuiting visitor should load only a.zsh and skip nested.zsh and b.zsh"
        );
    }
}
