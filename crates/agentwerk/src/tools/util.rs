//! Shared helpers for the file and shell tools — glob matching and process invocation.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::tool::{ToolContext, ToolResult};

/// Execute a command via `sh -c`, returning combined stdout/stderr. Bounded by
/// `timeout` and interruptible via the context's cancel signal: a fired
/// cancel drops the pending output future, and `kill_on_drop(true)` cascades
/// SIGKILL to the subprocess so a hanging `python3` / `sleep` doesn't outlive
/// the cancel.
pub(crate) async fn run_shell_command(
    command: &str,
    timeout: Duration,
    ctx: &ToolContext,
) -> ToolResult {
    let output_fut = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&ctx.dir)
        .kill_on_drop(true)
        .output();

    let result = tokio::select! {
        biased;
        _ = ctx.wait_for_cancel() => return ToolResult::error("Command cancelled"),
        r = tokio::time::timeout(timeout, output_fut) => r,
    };

    match result {
        Err(_) => ToolResult::error(format!("Command timed out after {}ms", timeout.as_millis())),
        Ok(Err(e)) => ToolResult::error(format!("Failed to execute command: {e}")),
        Ok(Ok(output)) => {
            let mut content = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr);

            if !stderr.is_empty() {
                content.push_str("\n--- stderr ---\n");
                content.push_str(&stderr);
            }

            if output.status.success() {
                ToolResult::success(content)
            } else {
                ToolResult::error(content)
            }
        }
    }
}

/// Simple glob matching supporting `*` (any chars) and `?` (single char).
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_bytes(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_bytes(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }

    match pattern[0] {
        b'*' => {
            glob_match_bytes(&pattern[1..], text)
                || (!text.is_empty() && glob_match_bytes(pattern, &text[1..]))
        }
        b'?' if text.is_empty() => false,
        b'?' => glob_match_bytes(&pattern[1..], &text[1..]),
        literal if text.is_empty() || text[0] != literal => false,
        _ => glob_match_bytes(&pattern[1..], &text[1..]),
    }
}

/// Cap on entries listed for a directory. A package dir can hold thousands of
/// files; an unbounded list would flood the model's context.
pub(crate) const MAX_DIR_ENTRIES: usize = 100;

/// Sorted entry names of `dir`, sub-directories marked with a trailing `/`,
/// capped at [`MAX_DIR_ENTRIES`] with a `… and N more` tail. Joined by `"\n  "`
/// so callers render `…:\n  {entries}`. `None` when the directory cannot be read.
pub(crate) fn directory_entries(dir: &Path) -> Option<String> {
    let read = std::fs::read_dir(dir).ok()?;
    let mut names: Vec<String> = read
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    names.sort();
    let total = names.len();
    let mut listed: Vec<String> = names.into_iter().take(MAX_DIR_ENTRIES).collect();
    if total > MAX_DIR_ENTRIES {
        listed.push(format!("… and {} more", total - MAX_DIR_ENTRIES));
    }
    Some(listed.join("\n  "))
}

/// First existing directory at or above `path` (`path` itself when it is a dir).
fn nearest_existing_dir(path: &Path) -> Option<&Path> {
    path.ancestors().find(|p| p.is_dir())
}

/// Recovery tail appended to a not-found error from a file tool. Prefers listing
/// the directory the model is guessing into — when it lies within the working
/// directory — so a wrong guess becomes the real directory contents next turn.
/// Falls back to echoing the working directory plus a dropped-folder suggestion
/// for paths that escape it.
pub(crate) fn not_found_hint(ctx_dir: &Path, resolved: &Path) -> String {
    if let Some(dir) = nearest_existing_dir(resolved) {
        if dir.starts_with(ctx_dir) {
            if let Some(entries) = directory_entries(dir) {
                return format!("'{}' contains:\n  {entries}", dir.display());
            }
        }
    }
    let cwd = ctx_dir.display();
    match suggest_path(ctx_dir, resolved) {
        Some(suggestion) => format!(
            "Note: your current working directory is {cwd}. Did you mean {}?",
            suggestion.display()
        ),
        None => format!("Note: your current working directory is {cwd}."),
    }
}

/// Dropped-folder heuristic: when the model built an absolute path that lands
/// outside the working directory, some trailing slice of it often exists *under*
/// the working directory (the model dropped or mis-nested leading folders).
/// Returns the longest trailing suffix that exists under `ctx_dir`; `None` when
/// the path is already under the working directory or no suffix matches.
fn suggest_path(ctx_dir: &Path, resolved: &Path) -> Option<PathBuf> {
    if resolved.starts_with(ctx_dir) {
        return None;
    }
    let segments: Vec<&std::ffi::OsStr> = resolved
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    // Longest trailing suffix first: drop the fewest leading folders that makes
    // the path resolve under the working directory.
    (0..segments.len()).find_map(|start| {
        let candidate: PathBuf = segments[start..]
            .iter()
            .fold(ctx_dir.to_path_buf(), |p, s| p.join(s));
        candidate.exists().then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    #[tokio::test]
    async fn cancel_interrupts_long_running_subprocess() {
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let flag = ctx.interrupt_signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            flag.store(true, Ordering::Relaxed);
        });

        let started = Instant::now();
        let result = run_shell_command("sleep 30", Duration::from_millis(60_000), &ctx).await;
        let elapsed = started.elapsed();

        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(
            matches!(result, ToolResult::Error(_)),
            "expected cancelled result"
        );
        assert!(content.contains("cancelled"));
        assert!(
            elapsed < Duration::from_millis(500),
            "cancel should return within 500ms, took {elapsed:?}",
        );
    }

    #[test]
    fn exact_match() {
        assert!(glob_match("hello", "hello"));
        assert!(!glob_match("hello", "world"));
    }

    #[test]
    fn star_wildcard() {
        assert!(glob_match("git *", "git status"));
        assert!(glob_match("git *", "git log --oneline"));
        assert!(!glob_match("git *", "cargo build"));
        assert!(!glob_match("git *", "git"));
    }

    #[test]
    fn question_mark_wildcard() {
        assert!(glob_match("?.rs", "a.rs"));
        assert!(!glob_match("?.rs", "ab.rs"));
    }

    #[test]
    fn match_all() {
        assert!(glob_match("*", "anything goes"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn suggest_path_recovers_dropped_working_directory_folder() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        let cwd = tmp.path().join("data83");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(cwd.join("flask.py"), "x = 1\n").unwrap();

        // Model dropped the `data83` folder: passed <parent>/flask.py.
        let resolved = tmp.path().join("flask.py");
        assert_eq!(suggest_path(&cwd, &resolved), Some(cwd.join("flask.py")));
    }

    #[test]
    fn suggest_path_none_when_path_is_under_working_directory() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        // A relative-resolved miss lands under cwd; dropped-folder does not apply.
        let resolved = tmp.path().join("missing.py");
        assert_eq!(suggest_path(tmp.path(), &resolved), None);
    }

    #[test]
    fn suggest_path_none_when_no_candidate_exists() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        let cwd = tmp.path().join("data83");
        std::fs::create_dir(&cwd).unwrap();
        // Outside cwd, but cwd/flask.py does not exist.
        let resolved = tmp.path().join("flask.py");
        assert_eq!(suggest_path(&cwd, &resolved), None);
    }

    #[test]
    fn not_found_hint_lists_the_in_tree_directory_being_guessed_into() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::write(pkg.join("helpers.py"), "x\n").unwrap();
        std::fs::write(pkg.join("cli.py"), "x\n").unwrap();

        // Model guessed pkg/pkg (package-name-as-file); pkg exists.
        let resolved = pkg.join("pkg");
        let hint = not_found_hint(tmp.path(), &resolved);
        assert!(
            hint.contains(&format!("'{}' contains", pkg.display())),
            "got {hint}"
        );
        assert!(
            hint.contains("cli.py") && hint.contains("helpers.py"),
            "got {hint}"
        );
    }

    #[test]
    fn not_found_hint_falls_back_to_suggestion_for_escaped_path() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        let cwd = tmp.path().join("data83");
        std::fs::create_dir_all(cwd.join("pkg")).unwrap();
        std::fs::write(cwd.join("pkg").join("setup.py"), "x\n").unwrap();

        // Escaped path (whole prefix dropped); the suffix exists under cwd.
        let resolved = Path::new("/data83/pkg/setup.py");
        let hint = not_found_hint(&cwd, resolved);
        assert!(
            hint.contains("your current working directory"),
            "got {hint}"
        );
        assert!(
            hint.contains("Did you mean") && hint.contains("pkg/setup.py"),
            "got {hint}"
        );
    }

    #[test]
    fn directory_entries_caps_large_directories() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        for i in 0..(MAX_DIR_ENTRIES + 5) {
            std::fs::write(tmp.path().join(format!("f{i:04}.txt")), "x").unwrap();
        }
        let body = directory_entries(tmp.path()).unwrap();
        assert!(body.contains("… and 5 more"), "should truncate, got {body}");
        // MAX_DIR_ENTRIES names plus the one "… and N more" line.
        assert_eq!(body.lines().count(), MAX_DIR_ENTRIES + 1);
    }

    #[test]
    fn suggest_path_recovers_multi_segment_dropped_prefix() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        let cwd = tmp.path().join("data83");
        std::fs::create_dir_all(cwd.join("quasarlib_1.0.8")).unwrap();
        std::fs::write(cwd.join("quasarlib_1.0.8").join("setup.py"), "x\n").unwrap();

        // Model dropped the whole working-directory prefix.
        let resolved = Path::new("/data83/quasarlib_1.0.8/setup.py");
        assert_eq!(
            suggest_path(&cwd, resolved),
            Some(cwd.join("quasarlib_1.0.8").join("setup.py"))
        );
    }
}
