//! Size-bounded rotating writer for the supervised Bee process's
//! captured stdout + stderr.
//!
//! The supervisor used to redirect Bee's log streams straight to a
//! `$TMPDIR/bee-tui-spawned-{ts}.log` file that grew without bound —
//! a long-running node could fill `$TMPDIR` overnight. This writer
//! caps the active file and rolls older content to numbered siblings
//! (`<base>.1`, `<base>.2`, …, `<base>.{keep_files}`); everything
//! beyond `keep_files` is unlinked.
//!
//! ## Why line-based, not byte-based
//!
//! Bee's logfmt entries are parsed one line at a time by
//! [`crate::bee_log::parse_line`]. Splitting an entry across the
//! rotation boundary would silently drop it (the old file's tail
//! would be a half-line, the new file's head another). We accept
//! a small byte over-shoot past `rotate_size_bytes` instead.
//!
//! ## Atomicity
//!
//! Rotation uses `std::fs::rename`, which is atomic on the same
//! filesystem (`$TMPDIR` and the rotation siblings always share a
//! filesystem). Concurrent writers aren't supported — the supervisor
//! spawns exactly one writer task per Bee child, so contention is
//! a non-concern here.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Append-with-rollover writer. Owns the active file fd; rotation
/// closes it, renames the path, and opens a fresh one.
pub struct BeeLogWriter {
    base_path: PathBuf,
    rotate_size_bytes: u64,
    keep_files: u32,
    current: File,
    current_size: u64,
}

impl BeeLogWriter {
    /// Open `base_path` for append, creating it if needed. If the
    /// file already exists (cockpit restart while Bee kept running)
    /// the existing size is taken into account — the next write past
    /// the cap will rotate it as expected.
    pub fn open(base_path: PathBuf, rotate_size_mb: u64, keep_files: u32) -> io::Result<Self> {
        let current = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&base_path)?;
        let current_size = current.metadata()?.len();
        Ok(Self {
            base_path,
            rotate_size_bytes: rotate_size_mb.saturating_mul(1024 * 1024).max(1),
            keep_files: keep_files.max(1),
            current,
            current_size,
        })
    }

    pub fn path(&self) -> &Path {
        &self.base_path
    }

    /// Append a single line (without trailing newline) to the active
    /// file, rotating first if it would push the file past the cap.
    /// The newline is added by the writer so callers don't need to
    /// remember it.
    pub fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let needed = line.len() as u64 + 1;
        // Rotate when the next line would push us over the cap, but
        // never on an empty active file — otherwise an oversized
        // first line would loop forever (rotate → empty → still
        // doesn't fit → rotate). Accept a small overshoot in that
        // pathological case.
        if self.current_size > 0 && self.current_size + needed > self.rotate_size_bytes {
            self.rotate()?;
        }
        self.current.write_all(line)?;
        self.current.write_all(b"\n")?;
        self.current_size += needed;
        Ok(())
    }

    /// Rotate `<base>` → `<base>.1`, `<base>.1` → `<base>.2`, …,
    /// dropping the oldest. Re-opens a fresh active file at
    /// `<base>` afterwards. Public for tests; production callers
    /// should rely on `write_line` triggering this automatically.
    pub fn rotate(&mut self) -> io::Result<()> {
        self.current.flush()?;
        // Discard the slot beyond keep_files (`<base>.{keep_files+1}`
        // would never be re-read). On Linux this is also a no-op when
        // the file doesn't exist.
        let oldest = path_with_suffix(&self.base_path, self.keep_files);
        let _ = std::fs::remove_file(&oldest);
        // Cascade .{N-1} → .{N}, …, .1 → .2.
        for i in (1..self.keep_files).rev() {
            let from = path_with_suffix(&self.base_path, i);
            let to = path_with_suffix(&self.base_path, i + 1);
            if from.exists() {
                std::fs::rename(&from, &to)?;
            }
        }
        // Active → .1.
        if self.base_path.exists() {
            let first = path_with_suffix(&self.base_path, 1);
            std::fs::rename(&self.base_path, &first)?;
        }
        // Open a fresh, truncated active file. `truncate(true)` is
        // belt-and-braces — the rename above already removed the
        // path's old content from the directory.
        self.current = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.base_path)?;
        self.current_size = 0;
        Ok(())
    }
}

/// Build `<base>.{n}` without losing the base extension. We can't
/// use `set_extension` because it would replace `.log` rather than
/// appending `.1` after it.
fn path_with_suffix(base: &Path, n: u32) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "bee-log-writer-test-{}.log",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn cleanup(base: &Path, keep_files: u32) {
        let _ = std::fs::remove_file(base);
        for i in 1..=keep_files + 1 {
            let _ = std::fs::remove_file(path_with_suffix(base, i));
        }
    }

    #[test]
    fn writes_lines_with_newlines_appended() {
        let path = unique_path();
        let mut w = BeeLogWriter::open(path.clone(), 1, 2).unwrap();
        w.write_line(b"hello").unwrap();
        w.write_line(b"world").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "hello\nworld\n");
        cleanup(&path, 2);
    }

    #[test]
    fn rotates_when_size_cap_exceeded() {
        // Cap = 1 MiB. Write 12 lines of ~100 KiB each so the second
        // batch forces a rotation. Verify .1 has the early lines and
        // active has the late ones.
        let path = unique_path();
        let mut w = BeeLogWriter::open(path.clone(), 1, 3).unwrap();
        let chunk: Vec<u8> = vec![b'x'; 100 * 1024]; // 100 KiB
        for _ in 0..15 {
            w.write_line(&chunk).unwrap();
        }
        // Active file should be smaller than cap (rotation happened
        // mid-loop). .1 must exist with the rolled-over content.
        let active_len = std::fs::metadata(&path).unwrap().len();
        assert!(
            active_len < 1024 * 1024,
            "active file should be under cap, got {active_len}"
        );
        let rotated_1 = path_with_suffix(&path, 1);
        assert!(rotated_1.exists(), ".1 rotation file must exist");
        cleanup(&path, 3);
    }

    #[test]
    fn rotation_drops_oldest_beyond_keep_count() {
        // keep_files=2 → only .1 and .2 ever exist; .3 should never
        // appear after multiple rotations.
        let path = unique_path();
        let mut w = BeeLogWriter::open(path.clone(), 1, 2).unwrap();
        let chunk: Vec<u8> = vec![b'y'; 600 * 1024]; // 600 KiB
        for _ in 0..10 {
            w.write_line(&chunk).unwrap();
        }
        assert!(path_with_suffix(&path, 1).exists(), ".1 must exist");
        assert!(path_with_suffix(&path, 2).exists(), ".2 must exist");
        assert!(
            !path_with_suffix(&path, 3).exists(),
            ".3 must NOT exist (keep_files=2)"
        );
        cleanup(&path, 3);
    }

    #[test]
    fn reopen_picks_up_existing_size() {
        // Operator restarts bee-tui while Bee keeps running — when
        // the writer re-opens an existing file it must factor the
        // existing size into the rotation cap so it doesn't suddenly
        // overshoot by another full cap's worth.
        let path = unique_path();
        std::fs::write(&path, "x".repeat(900_000)).unwrap();
        let mut w = BeeLogWriter::open(path.clone(), 1, 2).unwrap();
        // Just under the 1 MiB cap with this initial size — writing
        // 200 KiB should rotate.
        w.write_line(&vec![b'z'; 200 * 1024]).unwrap();
        assert!(
            path_with_suffix(&path, 1).exists(),
            "rotation should fire on the first write past the cap"
        );
        cleanup(&path, 2);
    }

    #[test]
    fn oversized_line_rotates_after_writing() {
        // A line bigger than the cap can't be split — write it as-is.
        // First call to `write_line` lands on an empty file (no rotate
        // yet); next call sees an over-cap file and rotates.
        let path = unique_path();
        let mut w = BeeLogWriter::open(path.clone(), 1, 2).unwrap();
        let huge: Vec<u8> = vec![b'h'; 2 * 1024 * 1024]; // 2 MiB
        w.write_line(&huge).unwrap();
        // Active file is over cap but no rotation yet (empty rule).
        assert!(std::fs::metadata(&path).unwrap().len() > 1024 * 1024);
        // Next small line triggers rotate.
        w.write_line(b"normal").unwrap();
        assert!(path_with_suffix(&path, 1).exists());
        cleanup(&path, 2);
    }
}
