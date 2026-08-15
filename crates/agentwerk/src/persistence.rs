//! How every value in agentwerk reads and writes its file.
//!
//! Each type encodes its own location, so the wrong file cannot be reached
//! through the wrong type.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) trait Persist: Sized {
    type Key;
    fn save(&self, dir: &Path) -> io::Result<()>;
    fn load(dir: &Path, key: &Self::Key) -> io::Result<Self>;
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    let temp = parent.join(format!(".{file_name}.tmp.{pid}.{counter}"));
    let result = (|| -> io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(crate) fn append_line(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

/// Relative path of a tool's output file under a tickets dir:
/// `tickets/<key>/outputs/<id>.txt`. Callers join with the tickets dir
/// to write; storing the relative form keeps the recorded paths portable.
pub(crate) fn output_path(key: &str, id: &str) -> PathBuf {
    PathBuf::from("tickets")
        .join(key)
        .join("outputs")
        .join(format!("{id}.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    #[test]
    fn write_atomic_creates_parent_and_writes_bytes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("file.txt");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn write_atomic_overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("f.txt");
        write_atomic(&path, b"v1").unwrap();
        write_atomic(&path, b"v2").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"v2");
    }

    #[test]
    fn append_line_creates_file_and_appends_newline() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.jsonl");
        append_line(&path, "first").unwrap();
        append_line(&path, "second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\nsecond\n");
    }
}
