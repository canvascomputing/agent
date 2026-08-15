//! Lets an agent search file contents by regular expression, so it can find
//! where something is mentioned before opening any one file.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use grep::searcher::sinks::UTF8;
use serde_json::{Map, Value};

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;
use crate::providers::ProviderResult as Result;

/// Search the working directory for a regular-expression `pattern` and return a
/// structured result: matching lines, matching file names, or per-file counts,
/// per `output_mode`. The body drives ripgrep's search engine in-process, so a
/// match is found the same way `rg pattern` would. Concurrent.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::GrepTool;
///
/// Agent::new().tool(GrepTool);
/// ```
pub struct GrepTool;

/// Default `head_limit`: how many results a call returns before truncating.
/// Extra results are dropped and `appliedLimit` is set so the model narrows
/// rather than re-runs.
const DEFAULT_HEAD_LIMIT: usize = 250;

/// Bytes of a single matched line the printer shows before truncating it. Minified
/// bundles produce megabyte-long lines; the preview keeps a single hit small.
pub(super) const MAX_LINE_COLUMNS: usize = 250;

/// How long a single search may run before it is killed. A regex over a huge
/// tree should still return promptly; a runaway pattern must not wedge a turn.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(180);

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("grep.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

impl ToolLike for GrepTool {
    fn name(&self) -> &str {
        &tool_file().name
    }

    fn description(&self) -> &str {
        description()
    }

    fn input_schema(&self) -> Value {
        tool_file().input_schema.clone()
    }

    fn is_concurrent(&self) -> bool {
        tool_file().concurrent
    }

    fn call<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
        Box::pin(async move {
            let query = match Query::from_input(&input) {
                Ok(query) => query,
                Err(response) => return Ok(response),
            };

            // The `grep`/`ignore` engine is synchronous, so run it on a blocking
            // thread: `grep` is parallel-callable and a 180s search must not stall
            // a runtime worker. The interrupt flag and deadline let it bail between
            // files, since a spawn_blocking task cannot be force-killed.
            let dir = ctx.dir.clone();
            let interrupt = Arc::new(AtomicBool::new(false));
            let searching = Arc::clone(&interrupt);
            let deadline = Instant::now() + SEARCH_TIMEOUT;
            let handle = tokio::task::spawn_blocking(move || {
                search_corpus(&dir, &query, &searching, deadline)
            });

            let outcome = tokio::select! {
                biased;
                _ = ctx.cancelled() => ToolResult::error("Search cancelled"),
                r = tokio::time::timeout(SEARCH_TIMEOUT, handle) => match r {
                    Err(_) => {
                        ToolResult::error(format!(
                            "Search timed out after {}s; narrow the pattern or path",
                            SEARCH_TIMEOUT.as_secs()
                        ))
                    }
                    Ok(Err(_)) => ToolResult::error("Search failed"),
                    Ok(Ok(result)) => result,
                },
            };
            // Whichever branch lost, the blocking thread is still walking files.
            interrupt.store(true, Ordering::Relaxed);
            Ok(outcome)
        })
    }
}

/// Which shape the result takes.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum OutputMode {
    Content,
    FilesWithMatches,
    Count,
}

/// Which matcher interprets `pattern`: ripgrep's regex, or the code-shape
/// engine reached via `syntax: "code"`.
#[derive(Clone, Copy, PartialEq)]
enum Syntax {
    Regex,
    Code,
}

impl OutputMode {
    fn name(self) -> &'static str {
        match self {
            OutputMode::Content => "content",
            OutputMode::FilesWithMatches => "files_with_matches",
            OutputMode::Count => "count",
        }
    }
}

/// The parsed, validated search parameters, kept apart from where and when the
/// search runs (the directory, interrupt flag, and deadline).
pub(super) struct Query {
    pub(super) pattern: String,
    path: Option<String>,
    glob: Option<String>,
    pub(super) output_mode: OutputMode,
    before_context: usize,
    after_context: usize,
    line_numbers: bool,
    pub(super) case_insensitive: bool,
    file_type: Option<String>,
    head_limit: usize,
    offset: usize,
    multiline: bool,
    syntax: Syntax,
    /// Raw `constraints` array, read only in code mode (validation needs the
    /// parsed pattern's metavariable names, so it is not resolved here).
    pub(super) constraints: Value,
}

impl Query {
    /// Read the model's arguments, applying defaults. Returns an error
    /// `ToolResult` (recoverable, not a hard failure) when `pattern` is missing.
    fn from_input(input: &Value) -> std::result::Result<Query, ToolResult> {
        let pattern = match input["pattern"].as_str() {
            Some(pattern) if !pattern.is_empty() => pattern.to_string(),
            _ => return Err(ToolResult::error("Missing required parameter: pattern")),
        };

        // `-C`/`context` set both sides; `-A`/`-B` override the near or far side.
        let both = number(input, "-C").or_else(|| number(input, "context"));
        let before_context = number(input, "-B").or(both).unwrap_or(0);
        let after_context = number(input, "-A").or(both).unwrap_or(0);

        let output_mode = match input["output_mode"].as_str() {
            Some("content") => OutputMode::Content,
            Some("count") => OutputMode::Count,
            _ => OutputMode::FilesWithMatches,
        };

        let syntax = match input["syntax"].as_str() {
            Some("code") => Syntax::Code,
            _ => Syntax::Regex,
        };

        Ok(Query {
            pattern,
            path: string(input, "path"),
            glob: string(input, "glob"),
            output_mode,
            before_context,
            after_context,
            line_numbers: input["-n"].as_bool().unwrap_or(true),
            case_insensitive: input["-i"].as_bool().unwrap_or(false),
            file_type: string(input, "type"),
            head_limit: number(input, "head_limit").unwrap_or(DEFAULT_HEAD_LIMIT),
            offset: number(input, "offset").unwrap_or(0),
            multiline: input["multiline"].as_bool().unwrap_or(false),
            syntax,
            constraints: input["constraints"].clone(),
        })
    }
}

fn number(input: &Value, key: &str) -> Option<usize> {
    input[key].as_u64().map(|n| n as usize)
}

fn string(input: &Value, key: &str) -> Option<String> {
    input[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Walk the corpus under `dir` with ripgrep's engine and return the structured
/// result for the requested `output_mode`. `standard_filters(false)` searches
/// every file, hidden and gitignored included, so the scan never makes a payload
/// invisible; narrow with `path`/`glob` instead. Runs on a blocking thread; the
/// interrupt flag and deadline let a long or cancelled search bail between files.
fn search_corpus(
    dir: &Path,
    query: &Query,
    interrupt: &AtomicBool,
    deadline: Instant,
) -> ToolResult {
    let root = match &query.path {
        Some(path) => dir.join(path),
        None => dir.to_path_buf(),
    };

    let mut walk = ignore::WalkBuilder::new(&root);
    walk.standard_filters(false);
    if let Some(glob) = query.glob.as_deref() {
        let mut builder = ignore::overrides::OverrideBuilder::new(&root);
        match builder.add(glob).and_then(|builder| builder.build()) {
            Ok(overrides) => {
                walk.overrides(overrides);
            }
            Err(error) => return ToolResult::error(format!("Invalid glob: {error}")),
        }
    }
    if let Some(file_type) = query.file_type.as_deref() {
        let mut builder = ignore::types::TypesBuilder::new();
        builder.add_defaults();
        builder.select(file_type);
        match builder.build() {
            Ok(types) => {
                walk.types(types);
            }
            Err(error) => {
                return ToolResult::error(format!("Unknown file type `{file_type}`: {error}"))
            }
        }
    }

    let files = collect_files(walk.build(), dir, interrupt, deadline);

    match query.syntax {
        Syntax::Regex => run_regex(&files, query, interrupt, deadline),
        Syntax::Code => super::code::run(&files, query, interrupt, deadline),
    }
}

/// Match `files` with ripgrep's regex engine and render per `output_mode`. The
/// regex matcher is built here, not in `search_corpus`, because a code search
/// never compiles one.
fn run_regex(
    files: &[(PathBuf, String)],
    query: &Query,
    interrupt: &AtomicBool,
    deadline: Instant,
) -> ToolResult {
    // Ripgrep's Rust-regex matcher, line-oriented. `dot_matches_new_line` and the
    // searcher's multi-line mode are tied to `multiline` so `.` spans newlines only
    // on request.
    let matcher = match grep::regex::RegexMatcherBuilder::new()
        .multi_line(true)
        .line_terminator(Some(b'\n'))
        .dot_matches_new_line(query.multiline)
        .case_insensitive(query.case_insensitive)
        .build(&query.pattern)
    {
        Ok(matcher) => matcher,
        // `pattern` is always a regex, so a raw literal like `os.system(` (unbalanced
        // paren) is invalid. Name the remedy so the caller escapes rather than
        // re-sending the same doomed pattern.
        Err(error) => {
            return ToolResult::error(format!(
                "Search failed: {error}. `pattern` is a regular expression. To find a call or \
                 code shape, use `syntax: \"code\"` (`Name(...)`); otherwise escape the \
                 metacharacters."
            ))
        }
    };

    let mut searcher = grep::searcher::SearcherBuilder::new();
    // The files/count sinks read the line number off each match, so line counting
    // stays on for them; content mode alone honours `-n` (`line_numbers`).
    let line_numbers = query.output_mode != OutputMode::Content || query.line_numbers;
    searcher
        .line_number(line_numbers)
        .multi_line(query.multiline);
    // Stop at the first NUL, as `rg` does, so a binary blob (a `.git` pack, an
    // image) never dumps raw bytes into the result. Hidden/gitignored *text* stays
    // searchable; only binary content is skipped.
    searcher.binary_detection(grep::searcher::BinaryDetection::quit(b'\x00'));
    if query.output_mode == OutputMode::Content {
        searcher
            .before_context(query.before_context)
            .after_context(query.after_context);
    }
    let mut searcher = searcher.build();

    // Enough results to fill the requested page (`offset..offset+head_limit`) plus
    // one to prove more exist; used to bound per-file and stop the walk early.
    let page_bound = query
        .offset
        .saturating_add(query.head_limit)
        .saturating_add(1);

    match query.output_mode {
        OutputMode::Content => {
            let mut printer = grep::printer::StandardBuilder::new()
                .column(true)
                .heading(false)
                .max_columns(Some(MAX_LINE_COLUMNS as u64))
                .max_columns_preview(true)
                .build_no_color(Vec::<u8>::new());
            for (path, display) in files {
                if interrupt.load(Ordering::Relaxed) || Instant::now() >= deadline {
                    break;
                }
                let mut sink = printer.sink_with_path(&matcher, display);
                let _ = searcher.search_path(&matcher, path, &mut sink);
            }
            let bytes = printer.into_inner().into_inner();
            render_content(&String::from_utf8_lossy(&bytes), query)
        }
        OutputMode::FilesWithMatches => {
            let mut hits = Vec::new();
            for (path, display) in files {
                if interrupt.load(Ordering::Relaxed) || Instant::now() >= deadline {
                    break;
                }
                let mut matched = false;
                let _ = searcher.search_path(
                    &matcher,
                    path,
                    UTF8(|_, _| {
                        matched = true;
                        Ok(false)
                    }),
                );
                if matched {
                    hits.push(display.clone());
                    if query.head_limit != 0 && hits.len() > page_bound {
                        break;
                    }
                }
            }
            render_files(&hits, query)
        }
        OutputMode::Count => {
            let mut rows = Vec::new();
            for (path, display) in files {
                if interrupt.load(Ordering::Relaxed) || Instant::now() >= deadline {
                    break;
                }
                let mut matches = 0u64;
                let _ = searcher.search_path(
                    &matcher,
                    path,
                    UTF8(|_, _| {
                        matches += 1;
                        Ok(true)
                    }),
                );
                if matches > 0 {
                    rows.push((display.clone(), matches));
                    if query.head_limit != 0 && rows.len() > page_bound {
                        break;
                    }
                }
            }
            render_count(&rows, query)
        }
    }
}

/// Collect the searchable files under the walk, relativized to `dir` for display.
/// Directory traversal is cheap; searching each file is where the deadline and
/// interrupt flag do their real work, so they are re-checked in the per-mode loop.
fn collect_files(
    walk: ignore::Walk,
    dir: &Path,
    interrupt: &AtomicBool,
    deadline: Instant,
) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    for result in walk {
        if interrupt.load(Ordering::Relaxed) || Instant::now() >= deadline {
            break;
        }
        let Ok(entry) = result else { continue };
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        let display = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();
        files.push((entry.into_path(), display));
    }
    files
}

/// The requested page of `rows` (`offset` then `head_limit`) plus whether the
/// limit dropped rows. `head_limit == 0` means unlimited.
fn paginate<'a, T>(rows: &'a [T], query: &Query) -> (&'a [T], bool) {
    let start = query.offset.min(rows.len());
    let end = if query.head_limit == 0 {
        rows.len()
    } else {
        (start + query.head_limit).min(rows.len())
    };
    let truncated = query.head_limit != 0 && rows.len() - start > query.head_limit;
    (&rows[start..end], truncated)
}

/// Add the `appliedLimit`/`appliedOffset` markers, each present only when it
/// actually shaped the result.
fn note_pagination(map: &mut Map<String, Value>, query: &Query, truncated: bool) {
    if truncated {
        map.insert("appliedLimit".into(), Value::from(query.head_limit as u64));
    }
    if query.offset > 0 {
        map.insert("appliedOffset".into(), Value::from(query.offset as u64));
    }
}

fn object_result(map: Map<String, Value>) -> ToolResult {
    ToolResult::success(serde_json::to_string(&Value::Object(map)).unwrap_or_default())
}

pub(super) fn render_content(text: &str, query: &Query) -> ToolResult {
    let lines: Vec<&str> = text.lines().collect();
    let (page, truncated) = paginate(&lines, query);

    let mut map = Map::new();
    map.insert("mode".into(), Value::from(query.output_mode.name()));
    map.insert("numFiles".into(), Value::from(0u64));
    map.insert("filenames".into(), Value::Array(Vec::new()));
    map.insert("content".into(), Value::from(page.join("\n")));
    map.insert("numLines".into(), Value::from(page.len() as u64));
    note_pagination(&mut map, query, truncated);
    object_result(map)
}

pub(super) fn render_files(hits: &[String], query: &Query) -> ToolResult {
    let (page, truncated) = paginate(hits, query);

    let mut map = Map::new();
    map.insert("mode".into(), Value::from(query.output_mode.name()));
    map.insert("numFiles".into(), Value::from(page.len() as u64));
    map.insert("filenames".into(), Value::from(page.to_vec()));
    note_pagination(&mut map, query, truncated);
    object_result(map)
}

pub(super) fn render_count(rows: &[(String, u64)], query: &Query) -> ToolResult {
    let (page, truncated) = paginate(rows, query);
    let content = page
        .iter()
        .map(|(path, count)| format!("{path}:{count}"))
        .collect::<Vec<_>>()
        .join("\n");
    let matches: u64 = page.iter().map(|(_, count)| count).sum();

    let mut map = Map::new();
    map.insert("mode".into(), Value::from(query.output_mode.name()));
    map.insert("numFiles".into(), Value::from(page.len() as u64));
    map.insert("filenames".into(), Value::Array(Vec::new()));
    map.insert("content".into(), Value::from(content));
    map.insert("numMatches".into(), Value::from(matches));
    note_pagination(&mut map, query, truncated);
    object_result(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_ctx(path: &std::path::Path) -> ToolContext {
        ToolContext::new(path.to_path_buf())
    }

    fn setup_test_dir() -> crate::test_util::TempDir {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(
            tmp.path().join("src/main.rs"),
            "fn main() {\n    println!(\"Hello world\");\n}\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("src/lib.rs"),
            "pub fn greet() {\n    println!(\"Hello\");\n}\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("readme.md"),
            "# Hello Project\nThis is a test.\n",
        )
        .unwrap();
        tmp
    }

    async fn search(ctx: &ToolContext, input: Value) -> Value {
        let result = GrepTool.call(input, ctx).await.unwrap();
        let content = result.content();
        serde_json::from_str(content).unwrap_or_else(|_| Value::String(content.to_string()))
    }

    #[tokio::test]
    async fn files_with_matches_is_the_default_mode() {
        let tmp = setup_test_dir();
        let ctx = test_ctx(tmp.path());
        let out = search(&ctx, serde_json::json!({"pattern": "Hello world"})).await;
        assert_eq!(out["mode"], "files_with_matches");
        let files = out["filenames"].as_array().unwrap();
        assert!(files.iter().any(|f| f == "src/main.rs"), "got {out}");
        // lib.rs / readme.md contain "Hello" but not "Hello world".
        assert!(!files.iter().any(|f| f == "src/lib.rs"), "got {out}");
        assert_eq!(out["numFiles"], 1);
    }

    #[tokio::test]
    async fn content_mode_reports_line_and_column() {
        let tmp = setup_test_dir();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({"pattern": "Hello world", "output_mode": "content"}),
        )
        .await;
        assert_eq!(out["mode"], "content");
        let content = out["content"].as_str().unwrap();
        // "Hello world" begins at column 15 of `    println!("Hello world");`.
        assert!(content.contains("src/main.rs:2:15:"), "got {content:?}");
        assert_eq!(out["numLines"], 1);
    }

    #[tokio::test]
    async fn pattern_is_a_regex() {
        let tmp = setup_test_dir();
        let ctx = test_ctx(tmp.path());
        // A regex, not a literal: `println!` with any argument.
        let out = search(
            &ctx,
            serde_json::json!({"pattern": r"println!\(.*\)", "output_mode": "files_with_matches"}),
        )
        .await;
        let files = out["filenames"].as_array().unwrap();
        assert!(files.iter().any(|f| f == "src/main.rs"), "got {out}");
        assert!(files.iter().any(|f| f == "src/lib.rs"), "got {out}");
    }

    #[tokio::test]
    async fn invalid_regex_names_the_escaping_remedy() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("payload.py"), "os.system(cmd)\n").unwrap();
        let ctx = test_ctx(tmp.path());
        // A raw literal `os.system(` is an invalid regex (unbalanced paren); the
        // error must point the caller at escaping.
        let out = search(&ctx, serde_json::json!({"pattern": "os.system("})).await;
        let message = out.as_str().unwrap_or_default();
        assert!(message.contains("escape"), "got {message:?}");
    }

    #[tokio::test]
    async fn escaped_literal_matches_the_call_verbatim() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("payload.py"), "os.system(cmd)\n").unwrap();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({"pattern": r"os\.system\(", "output_mode": "content"}),
        )
        .await;
        assert!(
            out["content"].as_str().unwrap().contains("payload.py:1:1:"),
            "got {out}"
        );
    }

    #[tokio::test]
    async fn case_insensitive_flag_matches_regardless_of_case() {
        let tmp = setup_test_dir();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({"pattern": "hello world", "-i": true}),
        )
        .await;
        assert_eq!(out["numFiles"], 1, "got {out}");
        // Default is case-sensitive, so the lowercase pattern misses.
        let sensitive = search(&ctx, serde_json::json!({"pattern": "hello world"})).await;
        assert_eq!(sensitive["numFiles"], 0, "got {sensitive}");
    }

    #[tokio::test]
    async fn glob_restricts_to_matching_paths() {
        let tmp = setup_test_dir();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({"pattern": "Hello", "glob": "src/*.rs"}),
        )
        .await;
        let files = out["filenames"].as_array().unwrap();
        assert!(files.iter().any(|f| f == "src/main.rs"), "got {out}");
        assert!(files.iter().any(|f| f == "src/lib.rs"), "got {out}");
        assert!(!files.iter().any(|f| f == "readme.md"), "got {out}");
    }

    #[tokio::test]
    async fn glob_brace_group_matches_several_extensions() {
        let tmp = setup_test_dir();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({"pattern": "Hello", "glob": "*.{rs,md}"}),
        )
        .await;
        let files = out["filenames"].as_array().unwrap();
        assert!(files.iter().any(|f| f == "src/main.rs"), "got {out}");
        assert!(files.iter().any(|f| f == "readme.md"), "got {out}");
    }

    #[tokio::test]
    async fn count_mode_totals_matches() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "needle\nneedle\n").unwrap();
        fs::write(tmp.path().join("b.txt"), "needle\n").unwrap();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({"pattern": "needle", "output_mode": "count"}),
        )
        .await;
        assert_eq!(out["mode"], "count");
        assert_eq!(out["numFiles"], 2, "got {out}");
        assert_eq!(out["numMatches"], 3, "got {out}");
    }

    #[tokio::test]
    async fn context_shows_surrounding_lines() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("f.txt"), "one\ntwo\nNEEDLE\nfour\nfive\n").unwrap();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({"pattern": "NEEDLE", "output_mode": "content", "-B": 1, "-A": 1}),
        )
        .await;
        let content = out["content"].as_str().unwrap();
        assert!(content.contains("two"), "before-context line: {content:?}");
        assert!(content.contains("four"), "after-context line: {content:?}");
    }

    #[tokio::test]
    async fn head_limit_and_offset_page_the_results() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        let body: String = (0..100).map(|i| format!("needle line {i}\n")).collect();
        fs::write(tmp.path().join("many.txt"), body).unwrap();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "head_limit": 5,
                "offset": 10,
            }),
        )
        .await;
        assert_eq!(out["numLines"], 5, "capped to head_limit: {out}");
        assert_eq!(out["appliedLimit"], 5, "truncation flagged: {out}");
        assert_eq!(out["appliedOffset"], 10, "offset flagged: {out}");
    }

    #[tokio::test]
    async fn type_filter_selects_by_language() {
        let tmp = setup_test_dir();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({"pattern": "Hello", "type": "rust"}),
        )
        .await;
        let files = out["filenames"].as_array().unwrap();
        assert!(
            files.iter().all(|f| f.as_str().unwrap().ends_with(".rs")),
            "got {out}"
        );
    }

    #[tokio::test]
    async fn binary_files_are_skipped() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        // A NUL byte marks the file binary; its matches must not reach the result.
        fs::write(tmp.path().join("blob.bin"), b"\x00\x00needle\x00\x00").unwrap();
        fs::write(tmp.path().join("code.txt"), "needle here\n").unwrap();
        let ctx = test_ctx(tmp.path());
        let out = search(&ctx, serde_json::json!({"pattern": "needle"})).await;
        let files = out["filenames"].as_array().unwrap();
        assert!(
            files.iter().any(|f| f == "code.txt"),
            "text file matched: {out}"
        );
        assert!(
            !files.iter().any(|f| f == "blob.bin"),
            "binary file skipped: {out}"
        );
    }

    #[tokio::test]
    async fn long_lines_are_truncated() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        let prefix = "x".repeat(1000);
        let suffix = "y".repeat(1000);
        fs::write(
            tmp.path().join("big.txt"),
            format!("{prefix}NEEDLE{suffix}"),
        )
        .unwrap();
        let ctx = test_ctx(tmp.path());
        let out = search(
            &ctx,
            serde_json::json!({"pattern": "NEEDLE", "output_mode": "content"}),
        )
        .await;
        let line = out["content"].as_str().unwrap().lines().next().unwrap();
        assert!(
            line.len() < 400,
            "long line truncated, got {} bytes",
            line.len()
        );
    }
}
