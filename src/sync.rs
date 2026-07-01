use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use notify_debouncer_mini::{
    new_debouncer,
    notify::{RecommendedWatcher, RecursiveMode},
    Debouncer,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error};

use crate::db::Db;
use crate::indexer::Indexer;

pub struct FileWatcher {
    _debouncer: Debouncer<RecommendedWatcher>,
}

impl FileWatcher {
    pub fn start(root: PathBuf, db: Arc<Mutex<Db>>) -> Result<Self> {
        let indexer = Arc::new(Indexer::new(db, root.clone()));
        let ignore = Arc::new(build_ignore(&root));

        let (tx, rx) = std::sync::mpsc::channel();

        let mut debouncer = new_debouncer(Duration::from_secs(2), move |result| {
            let _ = tx.send(result);
        })?;
        debouncer.watcher().watch(&root, RecursiveMode::Recursive)?;

        std::thread::spawn(move || {
            for result in rx {
                let events = match result {
                    Ok(events) => events,
                    Err(e) => {
                        error!("File watcher error: {:?}", e);
                        continue;
                    }
                };

                // Collapse the debounced batch into unique paths, partitioned
                // into updates (still on disk) and removals, so the whole burst
                // costs a single resolve pass.
                let mut updated = BTreeSet::new();
                let mut removed = BTreeSet::new();
                for event in events {
                    let path = event.path;
                    if !should_index(&path, &ignore) {
                        continue;
                    }
                    if path.exists() {
                        updated.insert(path);
                    } else {
                        removed.insert(path);
                    }
                }
                if updated.is_empty() && removed.is_empty() {
                    continue;
                }

                let updated: Vec<PathBuf> = updated.into_iter().collect();
                let removed: Vec<PathBuf> = removed.into_iter().collect();
                debug!(
                    "Watch batch: {} updated, {} removed",
                    updated.len(),
                    removed.len()
                );
                if let Err(e) = indexer.apply_changes(&updated, &removed) {
                    error!("Index update failed: {}", e);
                }
            }
        });

        Ok(Self {
            _debouncer: debouncer,
        })
    }
}

/// Build a gitignore matcher that covers nested `.gitignore` and
/// `.rusty-graphignore` files, so the watcher honours the same rules as the
/// indexer's `WalkBuilder`-based file collection.
fn build_ignore(root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    // Walk without any gitignore filtering to discover every gitignore file.
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .build();
    for entry in walker.flatten() {
        let name = entry.file_name();
        if name == ".gitignore" || name == ".rusty-graphignore" {
            let _ = builder.add(entry.path());
        }
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn should_index(path: &Path, ignore: &Gitignore) -> bool {
    use crate::types::{detect_language, is_ignored_path};
    if is_ignored_path(path) {
        return false;
    }
    if detect_language(path).is_none() {
        return false;
    }
    !ignore.matched_path_or_any_parents(path, false).is_ignore()
}

#[cfg(test)]
mod tests {
    use super::{build_ignore, should_index};
    use ignore::gitignore::Gitignore;
    use std::path::Path;

    #[test]
    fn should_index_skips_ignored_and_unsupported() {
        let none = Gitignore::empty();
        assert!(should_index(Path::new("/p/src/main.rs"), &none));
        assert!(!should_index(Path::new("/p/target/debug/main.rs"), &none));
        assert!(!should_index(Path::new("/p/.git/config"), &none));
        assert!(!should_index(Path::new("/p/README.md"), &none));
    }

    #[test]
    fn should_index_respects_codegraphignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".rusty-graphignore"), "generated/\n").unwrap();
        let gi = build_ignore(dir.path());
        let ignored = dir.path().join("generated").join("api.rs");
        let kept = dir.path().join("src").join("api.rs");
        assert!(!should_index(&ignored, &gi));
        assert!(should_index(&kept, &gi));
    }
}
