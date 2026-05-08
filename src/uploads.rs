//! Directory walker for `:upload-collection`. Recursively reads a
//! local directory and produces the `Vec<CollectionEntry>` that
//! `bee.file().upload_collection_entries(...)` consumes.
//!
//! ## Why a separate module
//!
//! `:upload-file` is one `tokio::fs::read` away from a working
//! upload — it doesn't need its own module. Collections are
//! different: traversal rules (skip hidden, skip symlinks),
//! caps (size + entry count), and tar-friendly path normalisation
//! all want unit tests against synthetic on-disk fixtures.
//! Keeping the walker pure (no Bee API, no tokio runtime) makes it
//! straightforward to test that.
//!
//! ## Path normalisation
//!
//! Tar entries within a collection use forward-slash separators
//! regardless of host OS — Bee resolves them to manifest forks
//! literally. We strip the input-dir prefix from each walked path,
//! convert backslashes to forward slashes (Windows safety), and
//! reject any path that escapes the root via `..` (defense in
//! depth — `WalkDir` shouldn't surface them when symlinks are
//! ignored, but we keep the explicit check).

use std::path::{Component, Path, PathBuf};

use bee::file::CollectionEntry;

/// Total bytes an `:upload-collection` invocation is allowed to
/// pack. Same ceiling as `:upload-file`: keeps the cockpit's event
/// loop responsive on operator-typical hardware. Operators with
/// larger payloads should drive `swarm-cli` out of process.
pub const MAX_COLLECTION_BYTES: u64 = 256 * 1024 * 1024;

/// Cap on entry count. A 10k-file collection is already extreme
/// for a TUI verb (the tar build is in-memory) but the cap is
/// generous enough that a typical static-site `dist/` directory
/// fits without question.
pub const MAX_COLLECTION_ENTRIES: usize = 10_000;

/// Result of a directory walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkedCollection {
    pub entries: Vec<CollectionEntry>,
    /// Sum of `entries[*].data.len()`.
    pub total_bytes: u64,
    /// Path of an `index.html` at the *collection root* (depth 1)
    /// when present — surfaced so the verb can auto-set
    /// `CollectionUploadOptions::index_document`. `None` when no
    /// such file exists; the operator can still pass an explicit
    /// `--index <path>` flag in a future iteration.
    pub default_index: Option<String>,
}

#[derive(Debug)]
pub enum WalkError {
    NotADirectory(PathBuf),
    TooManyEntries { cap: usize },
    TooLarge { cap: u64, observed: u64 },
    PathEscape(String),
    Io(std::io::Error),
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADirectory(p) => write!(f, "{} is not a directory", p.display()),
            Self::TooManyEntries { cap } => {
                write!(f, "collection exceeds {cap}-entry cap")
            }
            Self::TooLarge { cap, observed } => write!(
                f,
                "collection size {observed} bytes exceeds {} bytes ({} MiB) cap",
                cap,
                cap / (1024 * 1024)
            ),
            Self::PathEscape(p) => write!(f, "path {p:?} escapes the collection root"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for WalkError {}

impl From<std::io::Error> for WalkError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Walk `root` recursively, returning every regular file as a
/// [`CollectionEntry`]. Hidden files / directories (any path
/// component starting with `.`) and symlinks are skipped.
///
/// Pure (no Bee API). Errors are returned as [`WalkError`] so the
/// caller can format an operator-facing message.
pub fn walk_dir(root: &Path) -> Result<WalkedCollection, WalkError> {
    let meta = std::fs::metadata(root)?;
    if !meta.is_dir() {
        return Err(WalkError::NotADirectory(root.to_path_buf()));
    }
    let root_canonical = root.canonicalize()?;

    let mut entries: Vec<CollectionEntry> = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut default_index: Option<String> = None;

    walk_inner(
        &root_canonical,
        &root_canonical,
        &mut entries,
        &mut total_bytes,
        &mut default_index,
    )?;

    // Stable, deterministic order so tar output and the resulting
    // Swarm reference are reproducible across runs.
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(WalkedCollection {
        entries,
        total_bytes,
        default_index,
    })
}

fn walk_inner(
    root: &Path,
    dir: &Path,
    out: &mut Vec<CollectionEntry>,
    total: &mut u64,
    default_index: &mut Option<String>,
) -> Result<(), WalkError> {
    for ent in std::fs::read_dir(dir)? {
        let ent = ent?;
        let name = match ent.file_name().to_str().map(str::to_string) {
            Some(n) => n,
            None => continue, // non-UTF-8 names — skip silently
        };
        if name.starts_with('.') {
            continue;
        }
        let ft = ent.file_type()?;
        if ft.is_symlink() {
            continue; // safety — don't follow symlinks out of root
        }
        let abs = ent.path();
        if ft.is_dir() {
            walk_inner(root, &abs, out, total, default_index)?;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let rel = abs
            .strip_prefix(root)
            .map_err(|_| WalkError::PathEscape(abs.to_string_lossy().to_string()))?;
        // Reject any `..` traversal in the relative path.
        if rel.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(WalkError::PathEscape(rel.to_string_lossy().to_string()));
        }
        // Tar paths use forward slashes everywhere.
        let tar_path: String = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");

        let data = std::fs::read(&abs)?;
        let new_total = total.saturating_add(data.len() as u64);
        if new_total > MAX_COLLECTION_BYTES {
            return Err(WalkError::TooLarge {
                cap: MAX_COLLECTION_BYTES,
                observed: new_total,
            });
        }
        *total = new_total;

        if out.len() + 1 > MAX_COLLECTION_ENTRIES {
            return Err(WalkError::TooManyEntries {
                cap: MAX_COLLECTION_ENTRIES,
            });
        }

        if default_index.is_none() && tar_path == "index.html" {
            *default_index = Some(tar_path.clone());
        }

        out.push(CollectionEntry::new(tar_path, data));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("tempdir")
    }

    #[test]
    fn walk_empty_dir_returns_empty_collection() {
        let d = tmpdir("bee-tui-walk-empty");
        let r = walk_dir(d.path()).expect("walk ok");
        assert!(r.entries.is_empty());
        assert_eq!(r.total_bytes, 0);
        assert!(r.default_index.is_none());
    }

    #[test]
    fn walk_picks_up_files_and_normalises_paths() {
        let d = tmpdir("bee-tui-walk-paths");
        fs::create_dir_all(d.path().join("assets")).unwrap();
        fs::write(d.path().join("index.html"), b"<h1>hi</h1>").unwrap();
        fs::write(d.path().join("assets").join("logo.png"), [0u8; 16]).unwrap();
        let r = walk_dir(d.path()).expect("walk ok");
        let paths: Vec<&str> = r.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["assets/logo.png", "index.html"]);
        assert_eq!(r.total_bytes, 11 + 16);
        assert_eq!(r.default_index.as_deref(), Some("index.html"));
    }

    #[test]
    fn walk_skips_hidden_files_and_dirs() {
        let d = tmpdir("bee-tui-walk-hidden");
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::write(d.path().join(".git").join("HEAD"), b"x").unwrap();
        fs::write(d.path().join(".env"), b"x").unwrap();
        fs::write(d.path().join("visible.txt"), b"y").unwrap();
        let r = walk_dir(d.path()).expect("walk ok");
        let paths: Vec<&str> = r.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["visible.txt"]);
    }

    #[test]
    fn walk_does_not_follow_symlinks() {
        let d = tmpdir("bee-tui-walk-symlinks");
        let outside = tmpdir("bee-tui-walk-outside");
        fs::write(outside.path().join("secret.txt"), b"private").unwrap();
        fs::write(d.path().join("real.txt"), b"ok").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
        let r = walk_dir(d.path()).expect("walk ok");
        let paths: Vec<&str> = r.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["real.txt"]);
    }

    #[test]
    fn walk_errors_on_non_directory() {
        let d = tmpdir("bee-tui-walk-notdir");
        let f = d.path().join("file.txt");
        fs::write(&f, b"x").unwrap();
        match walk_dir(&f) {
            Err(WalkError::NotADirectory(_)) => {}
            other => panic!("expected NotADirectory, got {other:?}"),
        }
    }

    #[test]
    fn walk_default_index_only_at_root() {
        // index.html nested inside a subdirectory should NOT be
        // treated as the collection's default index.
        let d = tmpdir("bee-tui-walk-nested-index");
        fs::create_dir_all(d.path().join("docs")).unwrap();
        fs::write(d.path().join("docs").join("index.html"), b"x").unwrap();
        let r = walk_dir(d.path()).expect("walk ok");
        assert!(r.default_index.is_none());
    }

    #[test]
    fn walk_orders_entries_deterministically() {
        let d = tmpdir("bee-tui-walk-order");
        fs::write(d.path().join("z.txt"), b"x").unwrap();
        fs::write(d.path().join("a.txt"), b"x").unwrap();
        fs::write(d.path().join("m.txt"), b"x").unwrap();
        let r = walk_dir(d.path()).expect("walk ok");
        let paths: Vec<&str> = r.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "m.txt", "z.txt"]);
    }
}
