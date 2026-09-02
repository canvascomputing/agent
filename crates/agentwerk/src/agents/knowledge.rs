//! Durable memory an agent shares across tasks and with other agents, stored
//! as markdown pages on disk.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::persistence::{write_atomic, Persist};

const INDEX_FILE: &str = "index.md";
const PAGES_DIR: &str = "pages";
const DEFAULT_INDEX_CHAR_LIMIT: usize = 12000;
const DEFAULT_PAGE_TYPE: &str = "Knowledge";
const LEGACY_MEMORY_FILE: &str = "memory.jsonl";
const MIGRATED_SUFFIX: &str = ".migrated";
/// The OKF changelog filename. agentwerk never writes one, but the name is
/// reserved so a seeded bundle's `log.md` is not read in as a page.
const LOG_FILE: &str = "log.md";

/// One entry in the in-memory index.
#[derive(Clone, Debug)]
struct IndexEntry {
    slug: String,
    description: String,
    /// Bundle-root-relative path to the concept file, so a seeded page
    /// outside `pages/` stays readable and its index link resolves.
    path: String,
}

/// What can go wrong reading or writing knowledge pages. `Display` renders the
/// message the agent sees.
#[derive(Debug)]
pub enum KnowledgeError {
    /// A slug, description, or content value was rejected.
    PageRejected {
        /// Why the page was rejected.
        message: String,
    },
    /// No page exists at `slug`.
    PageNotFound {
        /// Slug that was not found.
        slug: String,
    },
    /// The underlying file operation failed.
    IoFailed {
        /// Operation that failed.
        message: String,
        /// Underlying I/O error.
        source: io::Error,
    },
}

impl fmt::Display for KnowledgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageRejected { message } => write!(f, "{message}"),
            Self::PageNotFound { slug } => write!(f, "Page `{slug}` not found"),
            Self::IoFailed { message, source } => write!(f, "{message}: {source}"),
        }
    }
}

impl std::error::Error for KnowledgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IoFailed { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn io_failed(message: impl Into<String>) -> impl FnOnce(io::Error) -> KnowledgeError {
    let message = message.into();
    |source| KnowledgeError::IoFailed { message, source }
}

/// Store durable knowledge pages that agents share across tasks.
///
/// Pages are created in the Open Knowledge Format (OKF) v0.1 under
/// `<dir>/pages/<slug>.md`, each carrying `type`, `description`, and `timestamp`
/// frontmatter. `<dir>/index.md` lists them in one line each, and that list, or
/// as much of it as fits, is what the agent's prompt shows.
///
/// Two agents bound to one store share it:
///
/// ```no_run
/// use agentwerk::{Agent, Knowledge};
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let store = Knowledge::load("./.agentwerk/knowledge")?;
/// let alice = Agent::new().knowledge(&store);
/// let bob = Agent::new().knowledge(&store);
/// # let _ = (alice, bob);
/// # Ok(())
/// # }
/// ```
///
/// Because the store is a plain OKF bundle, an agent can be seeded from one a
/// human or another tool authored:
///
/// ```no_run
/// use agentwerk::{Agent, Knowledge};
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let seeded = Knowledge::load("./known-signatures")?;
/// let scanner = Agent::new().knowledge(&seeded);
/// # let _ = scanner;
/// # Ok(())
/// # }
/// ```
pub struct Knowledge {
    knowledge_dir: PathBuf,
    index: Mutex<Vec<IndexEntry>>,
    write_lock: Mutex<()>,
    index_char_limit: AtomicUsize,
}

impl Knowledge {
    /// Open the knowledge store at `knowledge_dir`, or create one there.
    ///
    /// The index is rebuilt from the pages themselves, so dropping an existing
    /// OKF bundle at `knowledge_dir` seeds the store from it. A `memory.jsonl`
    /// left by an older version is moved once into `pages/legacy-notes.md`.
    pub fn load(knowledge_dir: impl Into<PathBuf>) -> io::Result<Arc<Self>> {
        let knowledge_dir = knowledge_dir.into();
        fs::create_dir_all(knowledge_dir.join(PAGES_DIR))?;

        let legacy_path = knowledge_dir.join(LEGACY_MEMORY_FILE);

        // Page frontmatter is the source of truth: rebuild the in-memory index
        // by walking the bundle and regenerate the derived `index.md`. A legacy
        // migration runs first, made idempotent by its own `.migrated` rename.
        let index = if legacy_path.exists() {
            migrate_memory_jsonl(&knowledge_dir)?
        } else {
            rebuild_index_from_pages(&knowledge_dir)?
        };

        Ok(Arc::new(Self {
            knowledge_dir,
            index: Mutex::new(index),
            write_lock: Mutex::new(()),
            index_char_limit: AtomicUsize::new(DEFAULT_INDEX_CHAR_LIMIT),
        }))
    }

    /// Limit how much of the index is injected into the prompt, 12 000
    /// characters by default.
    ///
    /// No write is refused for being too large: past the limit the prompt lists
    /// the pages that fit and names `index.md` for the rest. Page bodies are
    /// never limited. Set it on the loaded store before handing the store to any
    /// agent.
    pub fn set_index_char_limit(&self, count: usize) -> &Self {
        self.index_char_limit.store(count, Ordering::Relaxed);
        self
    }

    /// Get the index size limit in force, 12 000 until
    /// [`Self::set_index_char_limit`] changes it.
    pub fn get_index_char_limit(&self) -> usize {
        self.index_char_limit.load(Ordering::Relaxed)
    }

    /// Get the index, which is injected into the agent prompt. Empty while the
    /// store holds no pages.
    ///
    /// Past [`Self::set_index_char_limit`] the listing stops at the last page that
    /// fits and a directive names `index.md` for the agent to read. That
    /// directive is a floor: a limit too small to hold it still gets it.
    pub fn get_index(&self) -> String {
        let index = self.index.lock().unwrap();
        let listing = render_index(&index);
        let limit = self.get_index_char_limit();
        if listing.len() <= limit {
            return listing;
        }
        render_limited_index(&index, limit, &self.index_path())
    }

    /// The whole index, however long it runs. `KnowledgeTool`'s `list`
    /// shows every page even when the prompt only had room for some.
    pub(crate) fn full_index(&self) -> String {
        let index = self.index.lock().unwrap();
        render_index(&index)
    }

    /// Where `index.md` sits, absolute so the path resolves whatever directory
    /// the agent's tools run in.
    fn index_path(&self) -> PathBuf {
        fs::canonicalize(&self.knowledge_dir)
            .unwrap_or_else(|_| self.knowledge_dir.clone())
            .join(INDEX_FILE)
    }

    /// Get the page collection for reading and writing pages.
    pub fn get_pages(&self) -> Pages<'_> {
        Pages { inner: self }
    }

    /// Remove every page from the store.
    pub fn clear(&self) -> Result<(), KnowledgeError> {
        let _w = self.write_lock.lock().unwrap();

        let pages_dir = self.knowledge_dir.join(PAGES_DIR);
        if pages_dir.exists() {
            for entry in fs::read_dir(&pages_dir).map_err(io_failed("Failed to list pages"))? {
                let entry = entry.map_err(io_failed("Failed to list pages"))?;
                if entry.path().extension().is_some_and(|ext| ext == "md") {
                    fs::remove_file(entry.path())
                        .map_err(io_failed("Failed to remove page file"))?;
                }
            }
        }

        let index_path = self.knowledge_dir.join(INDEX_FILE);
        if index_path.exists() {
            fs::remove_file(&index_path).map_err(io_failed("Failed to remove index"))?;
        }

        *self.index.lock().unwrap() = Vec::new();
        Ok(())
    }

    /// How large the index is, how large it may get, and how many pages it
    /// holds. `KnowledgeTool` shows the agent what a write consumed.
    pub(crate) fn index_usage(&self) -> (usize, usize, usize) {
        let index = self.index.lock().unwrap();
        (
            render_index(&index).len(),
            self.get_index_char_limit(),
            index.len(),
        )
    }
}

/// A `Page` is one thing an agent learned, stored as markdown under
/// `<dir>/pages/<slug>.md` with its frontmatter above the body.
#[derive(Debug, Clone)]
pub struct Page {
    /// Stable page identifier and filename stem.
    pub slug: String,
    /// What kind of page this is, `Knowledge` by default.
    pub kind: String,
    /// Short description shown in the knowledge index.
    pub description: String,
    /// Markdown page body.
    pub content: String,
    /// Terms associated with the page.
    pub tags: Vec<String>,
}

impl Page {
    /// Return the page slug.
    pub fn get_slug(&self) -> &str {
        &self.slug
    }

    /// Return the page kind.
    pub fn get_kind(&self) -> &str {
        &self.kind
    }

    /// Return the index description.
    pub fn get_description(&self) -> &str {
        &self.description
    }

    /// Return the markdown content.
    pub fn get_content(&self) -> &str {
        &self.content
    }

    /// Return the page tags.
    pub fn get_tags(&self) -> &[String] {
        &self.tags
    }
}

impl Persist for Page {
    type Key = String;

    fn save(&self, dir: &Path) -> io::Result<()> {
        let body = render_page(&self.kind, &self.description, &self.content, &self.tags);
        write_atomic(&page_path(dir, &self.slug), body.as_bytes())
    }

    fn load(dir: &Path, slug: &Self::Key) -> io::Result<Self> {
        let raw = fs::read_to_string(page_path(dir, slug))?;
        let (kind, description, tags, content) = parse_page(&raw);
        Ok(Page {
            slug: slug.clone(),
            kind,
            description,
            content,
            tags,
        })
    }
}

/// The page collection, returned by [`Knowledge::get_pages`]. Every write through
/// it updates the index; none is refused for the size of that index.
pub struct Pages<'a> {
    inner: &'a Knowledge,
}

impl Pages<'_> {
    /// Create or replace a page, and its entry in the index.
    pub fn save(&self, page: Page) -> Result<(), KnowledgeError> {
        let slug = normalize_slug(&page.slug)?;
        let description = page.description.trim();
        if description.is_empty() {
            return Err(KnowledgeError::PageRejected {
                message: "Description must not be empty".into(),
            });
        }
        if page.content.trim().is_empty() {
            return Err(KnowledgeError::PageRejected {
                message: "Content must not be empty".into(),
            });
        }
        let kind = match page.kind.trim() {
            "" => DEFAULT_PAGE_TYPE.to_string(),
            k => k.to_string(),
        };

        let _w = self.inner.write_lock.lock().unwrap();
        let mut index = self.inner.index.lock().unwrap().clone();

        let entry = IndexEntry {
            slug: slug.clone(),
            description: description.to_string(),
            path: format!("{PAGES_DIR}/{slug}.md"),
        };
        if let Some(pos) = index.iter().position(|e| e.slug == slug) {
            index[pos] = entry;
        } else {
            index.push(entry);
        }

        let normalized = Page {
            slug,
            kind,
            description: description.to_string(),
            content: page.content,
            tags: page.tags,
        };
        normalized
            .save(&self.inner.knowledge_dir)
            .map_err(io_failed("Failed to write page"))?;

        let index_body = render_index_file(&index);
        write_atomic(
            &self.inner.knowledge_dir.join(INDEX_FILE),
            index_body.as_bytes(),
        )
        .map_err(io_failed("Failed to write index"))?;

        *self.inner.index.lock().unwrap() = index;
        Ok(())
    }

    /// Read one page by its slug.
    ///
    /// The index says where the file is, so a seeded page kept outside `pages/`
    /// is still readable.
    pub fn get_page(&self, slug: &str) -> Result<Page, KnowledgeError> {
        let slug = normalize_slug(slug)?;
        let relative = self
            .inner
            .index
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.slug == slug)
            .map(|e| e.path.clone());
        let path = match relative {
            Some(rel) => self.inner.knowledge_dir.join(rel),
            None => page_path(&self.inner.knowledge_dir, &slug),
        };
        if !path.exists() {
            return Err(KnowledgeError::PageNotFound { slug });
        }
        let raw = fs::read_to_string(&path)
            .map_err(io_failed(format!("Failed to read page `{slug}`")))?;
        let (kind, description, tags, content) = parse_page(&raw);
        Ok(Page {
            slug,
            kind,
            description,
            content,
            tags,
        })
    }

    /// Get every page in the store, in index order.
    pub fn get_all(&self) -> Result<Vec<Page>, KnowledgeError> {
        let slugs: Vec<String> = self
            .inner
            .index
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.slug.clone())
            .collect();
        slugs.iter().map(|slug| self.get_page(slug)).collect()
    }

    /// Remove one page and its entry in the index.
    pub fn remove(&self, slug: &str) -> Result<(), KnowledgeError> {
        let slug = normalize_slug(slug)?;
        let _w = self.inner.write_lock.lock().unwrap();
        let mut index = self.inner.index.lock().unwrap().clone();

        let pos = index
            .iter()
            .position(|e| e.slug == slug)
            .ok_or(KnowledgeError::PageNotFound { slug })?;
        let removed = index.remove(pos);

        let page_file = self.inner.knowledge_dir.join(&removed.path);
        if page_file.exists() {
            fs::remove_file(&page_file).map_err(io_failed("Failed to remove page file"))?;
        }

        let index_body = render_index_file(&index);
        write_atomic(
            &self.inner.knowledge_dir.join(INDEX_FILE),
            index_body.as_bytes(),
        )
        .map_err(io_failed("Failed to write index"))?;

        *self.inner.index.lock().unwrap() = index;
        Ok(())
    }
}

fn page_path(knowledge_dir: &Path, slug: &str) -> PathBuf {
    knowledge_dir.join(PAGES_DIR).join(format!("{slug}.md"))
}

// Slug validation

/// Turn any string into a slug: lowercase `[a-z0-9-]`, no hyphen at either
/// end, at most 60 characters.
///
/// Every other character becomes a hyphen, runs of hyphens collapse, and a file
/// extension is dropped.
pub(crate) fn normalize_slug(raw: &str) -> Result<String, KnowledgeError> {
    let slug: String = raw
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();

    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_hyphen = true;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                collapsed.push('-');
            }
            prev_hyphen = true;
        } else {
            collapsed.push(c);
            prev_hyphen = false;
        }
    }
    while collapsed.ends_with('-') {
        collapsed.pop();
    }

    if collapsed.is_empty() {
        return Err(KnowledgeError::PageRejected {
            message: "Slug must not be empty".into(),
        });
    }

    if collapsed.len() > 60 {
        collapsed.truncate(60);
        if let Some(last_hyphen) = collapsed.rfind('-') {
            collapsed.truncate(last_hyphen);
        }
        while collapsed.ends_with('-') {
            collapsed.pop();
        }
    }

    if collapsed.is_empty() {
        return Err(KnowledgeError::PageRejected {
            message: "Slug must not be empty".into(),
        });
    }

    Ok(collapsed)
}

// Frontmatter

fn render_page(kind: &str, description: &str, content: &str, tags: &[String]) -> String {
    let now = format_iso8601_now();
    let tags_str = if tags.is_empty() {
        String::new()
    } else {
        format!(
            "\ntags: [{}]",
            tags.iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    // OKF v0.1 reserved keys: `type` (required), `description`, `timestamp`.
    format!("---\ntype: {kind}\ndescription: {description}\ntimestamp: {now}{tags_str}\n---\n{content}\n")
}

/// Read a page file into `(kind, description, tags, content)`. A missing `type`
/// reads as `Knowledge`, and the older `summary` key still reads as
/// `description`.
fn parse_page(raw: &str) -> (String, String, Vec<String>, String) {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return (
            DEFAULT_PAGE_TYPE.to_string(),
            String::new(),
            Vec::new(),
            raw.to_string(),
        );
    }
    let after_first = &trimmed[3..];
    let Some(end) = after_first.find("\n---") else {
        return (
            DEFAULT_PAGE_TYPE.to_string(),
            String::new(),
            Vec::new(),
            raw.to_string(),
        );
    };
    let frontmatter = &after_first[..end];
    let rest = &after_first[end + 4..];
    let content = rest.strip_prefix('\n').unwrap_or(rest).to_string();

    let mut kind = String::new();
    let mut description = String::new();
    let mut tags = Vec::new();
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("type:") {
            kind = value.trim().to_string();
        } else if let Some(value) = line
            .strip_prefix("description:")
            .or_else(|| line.strip_prefix("summary:"))
        {
            description = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("tags:") {
            let body = value.trim().trim_start_matches('[').trim_end_matches(']');
            tags = body
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }
    if kind.is_empty() {
        kind = DEFAULT_PAGE_TYPE.to_string();
    }
    (kind, description, tags, content)
}

#[cfg(test)]
fn strip_frontmatter(raw: &str) -> String {
    parse_page(raw).3
}

// Index rendering / parsing

/// One page in the index the prompt shows. The agent reads a page by its slug,
/// so each slug stays visible.
fn render_entry(entry: &IndexEntry) -> String {
    format!("- **{}**: {}", entry.slug, entry.description)
}

fn render_index(entries: &[IndexEntry]) -> String {
    entries
        .iter()
        .map(render_entry)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pages that fit, closed by the directive naming the file that lists
/// them all.
fn render_limited_index(entries: &[IndexEntry], limit: usize, path: &Path) -> String {
    // Charged at the whole store's count, which only shrinks as pages are
    // listed, so the reservation cannot fall short.
    let mut used = index_directive(entries.len(), path).len() + 1;

    let mut listed = Vec::new();
    for entry in entries {
        let line = render_entry(entry);
        let cost = line.len() + 1;
        // Stop rather than cut: half a line names a slug the agent cannot read.
        if used + cost > limit {
            break;
        }
        used += cost;
        listed.push(line);
    }

    let directive = index_directive(entries.len() - listed.len(), path);
    if listed.is_empty() {
        return directive;
    }
    format!("{}\n\n{directive}", listed.join("\n"))
}

/// Sends the agent to the file that does list every page.
fn index_directive(remaining: usize, path: &Path) -> String {
    let pages = if remaining == 1 {
        "page is"
    } else {
        "pages are"
    };
    crate::prompts::directives::built_in(
        crate::prompts::directives::KNOWLEDGE_INDEX_TRUNCATED,
        &[
            ("remaining", &remaining.to_string()),
            ("pages", pages),
            ("path", &path.display().to_string()),
        ],
    )
}

/// The `index.md` written to disk, one clickable link per page. It is derived
/// and never read back: the pages themselves are the source of truth.
fn render_index_file(entries: &[IndexEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let body = entries
        .iter()
        .map(|e| format!("* [{}]({}) - {}", e.slug, e.path, e.description))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{body}\n")
}

/// Rebuild the index from the pages on disk, then write `index.md` again. This
/// is what lets a bundle authored elsewhere seed the store.
fn rebuild_index_from_pages(knowledge_dir: &Path) -> io::Result<Vec<IndexEntry>> {
    let mut entries = Vec::new();
    collect_pages(knowledge_dir, knowledge_dir, &mut entries)?;
    entries.sort_by(|a, b| a.slug.cmp(&b.slug));

    if !entries.is_empty() {
        let index_body = render_index_file(&entries);
        write_atomic(&knowledge_dir.join(INDEX_FILE), index_body.as_bytes())?;
    }
    Ok(entries)
}

/// Turn every markdown file under `dir` into an index entry, taking the
/// description from the frontmatter or, without one, from the body's first
/// heading.
fn collect_pages(root: &Path, dir: &Path, entries: &mut Vec<IndexEntry>) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_pages(root, &path, entries)?;
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if is_reserved(&file_name) {
            continue;
        }
        let Ok(slug) = bundle_slug(root, &path) else {
            continue;
        };
        if entries.iter().any(|e| e.slug == slug) {
            continue;
        }
        let raw = fs::read_to_string(&path)?;
        let (_kind, description, _tags, content) = parse_page(&raw);
        let description = if description.is_empty() {
            extract_h1_summary(&content)
        } else {
            description
        };
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        entries.push(IndexEntry {
            slug,
            description,
            path: relative,
        });
    }
    Ok(())
}

/// Reserved OKF filenames and migrated legacy files never become concepts.
fn is_reserved(file_name: &str) -> bool {
    file_name == INDEX_FILE || file_name == LOG_FILE || file_name.ends_with(MIGRATED_SUFFIX)
}

/// The slug for a page file: its path without `.md`, and without a leading
/// `pages/`, so a page written by an agent keeps the file's own name.
fn bundle_slug(root: &Path, path: &Path) -> Result<String, KnowledgeError> {
    let relative = path.strip_prefix(root).unwrap_or(path).with_extension("");
    let mut text = relative.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = text.strip_prefix(&format!("{PAGES_DIR}/")) {
        text = stripped.to_string();
    }
    normalize_slug(&text)
}

fn extract_h1_summary(body: &str) -> String {
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("# ") {
            return heading.trim().to_string();
        }
    }
    "(no summary)".to_string()
}

// Migration from memory.jsonl

#[derive(serde::Deserialize)]
struct LegacyMemoryRecord {
    content: String,
    #[allow(dead_code)]
    added_at: u64,
}

fn migrate_memory_jsonl(knowledge_dir: &Path) -> io::Result<Vec<IndexEntry>> {
    let legacy_path = knowledge_dir.join(LEGACY_MEMORY_FILE);
    let raw = fs::read_to_string(&legacy_path)?;

    let mut contents: Vec<String> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<LegacyMemoryRecord>(trimmed) {
            if !record.content.is_empty() {
                contents.push(record.content);
            }
        }
    }

    let entries = if contents.is_empty() {
        Vec::new()
    } else {
        let page_content = contents
            .iter()
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n");
        let page_body = format!("# Legacy Notes\n\n{page_content}");
        let slug = "legacy-notes";
        let summary = format!("Migrated from memory.jsonl ({} entries)", contents.len());

        let page_file = render_page(DEFAULT_PAGE_TYPE, &summary, &page_body, &[]);
        let page_path = knowledge_dir.join(PAGES_DIR).join(format!("{slug}.md"));
        write_atomic(&page_path, page_file.as_bytes())?;

        let entry = IndexEntry {
            slug: slug.to_string(),
            description: summary,
            path: format!("{PAGES_DIR}/{slug}.md"),
        };
        vec![entry]
    };

    let index_body = render_index_file(&entries);
    write_atomic(&knowledge_dir.join(INDEX_FILE), index_body.as_bytes())?;

    let migrated_path = knowledge_dir.join(format!("{LEGACY_MEMORY_FILE}{MIGRATED_SUFFIX}"));
    fs::rename(&legacy_path, &migrated_path)?;

    Ok(entries)
}

// Timestamp

/// An ISO 8601 UTC timestamp, computed the same way as
/// `prompts::format_current_date` and extended with the time of day.
fn format_iso8601_now() -> String {
    let epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let secs_of_day = epoch_secs % 86400;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    let days = epoch_secs / 86400;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_store() -> (Arc<Knowledge>, crate::test_util::TempDir) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        (store, dir)
    }

    fn save_page(store: &Knowledge, slug: &str, description: &str, content: &str, tags: &[&str]) {
        let page = Page {
            slug: slug.to_string(),
            kind: String::new(),
            description: description.to_string(),
            content: content.to_string(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
        };
        store.get_pages().save(page).unwrap()
    }

    #[test]
    fn page_getters_expose_every_field() {
        let page = Page {
            slug: "build".into(),
            kind: "Guide".into(),
            description: "How to build.".into(),
            content: "Run make.".into(),
            tags: vec!["build".into(), "local".into()],
        };
        assert_eq!(page.get_slug(), "build");
        assert_eq!(page.get_kind(), "Guide");
        assert_eq!(page.get_description(), "How to build.");
        assert_eq!(page.get_content(), "Run make.");
        assert_eq!(page.get_tags(), &["build", "local"]);
    }

    #[test]
    fn load_creates_pages_directory_at_the_bundle_root() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let nested = dir.path().join("not-yet-there");
        let _ = Knowledge::load(&nested).unwrap();
        assert!(nested.join(PAGES_DIR).exists());
        assert!(!nested.join("knowledge").exists());
    }

    #[test]
    fn pages_at_the_bundle_root_are_indexed() {
        let dir = crate::test_util::TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(PAGES_DIR)).unwrap();
        fs::write(
            dir.path().join(PAGES_DIR).join("stray.md"),
            "---\ndescription: Beside the bundle.\n---\n# Stray\n\nBody.",
        )
        .unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        assert!(store.get_index().contains("stray"));
    }

    #[test]
    fn load_with_no_existing_files_starts_empty() {
        let (store, _dir) = fresh_store();
        assert!(store.get_index().is_empty());
    }

    #[test]
    fn save_page_creates_file_and_index() {
        let (store, dir) = fresh_store();
        save_page(
            &store,
            "deploy-config",
            "Staging on port 8080",
            "# Deploy\n\nPort 8080.",
            &[],
        );
        assert!(dir.path().join(PAGES_DIR).join("deploy-config.md").exists());
        assert!(dir.path().join(INDEX_FILE).exists());
        let idx = store.get_index();
        assert!(idx.contains("deploy-config"));
        assert!(idx.contains("Staging on port 8080"));
    }

    #[test]
    fn save_page_replaces_existing_entry() {
        let (store, _dir) = fresh_store();
        save_page(&store, "config", "v1", "# Config\n\nVersion 1.", &[]);
        save_page(&store, "config", "v2", "# Config\n\nVersion 2.", &[]);
        let idx = store.get_index();
        assert!(idx.contains("v2"));
        assert!(!idx.contains("v1"));
        assert_eq!(idx.lines().count(), 1);
    }

    #[test]
    fn load_page_returns_body_without_frontmatter() {
        let (store, _dir) = fresh_store();
        save_page(
            &store,
            "test",
            "A test page",
            "# Test\n\nHello world.",
            &["tag1"],
        );
        let body = store
            .get_pages()
            .get_page("test")
            .map(|p| p.content)
            .unwrap();
        assert!(body.contains("# Test"));
        assert!(body.contains("Hello world."));
        assert!(!body.contains("---"));
        assert!(!body.contains("timestamp:"));
        assert!(!body.contains("tags:"));
    }

    #[test]
    fn load_page_not_found() {
        let (store, _dir) = fresh_store();
        let err = store.get_pages().get_page("nonexistent").unwrap_err();
        assert!(matches!(err, KnowledgeError::PageNotFound { .. }));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn list_returns_every_saved_page_in_index_order() {
        let (store, _dir) = fresh_store();
        save_page(&store, "build", "How to build", "# Build\n\nRun make.", &[]);
        save_page(&store, "deploy", "How to deploy", "# Deploy\n\nPush.", &[]);

        let pages = store.get_pages().get_all().unwrap();
        let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(slugs, vec!["build", "deploy"]);
        assert_eq!(pages[0].description, "How to build");
        assert!(pages[0].content.contains("Run make."));
    }

    #[test]
    fn get_index_char_limit_returns_the_default_until_it_is_set() {
        let (store, _dir) = fresh_store();
        assert_eq!(store.get_index_char_limit(), DEFAULT_INDEX_CHAR_LIMIT);
        store.set_index_char_limit(80);
        assert_eq!(store.get_index_char_limit(), 80);
    }

    #[test]
    fn list_is_empty_on_a_fresh_store() {
        let (store, _dir) = fresh_store();
        assert!(store.get_pages().get_all().unwrap().is_empty());
    }

    #[test]
    fn remove_page_clears_file_and_index_entry() {
        let (store, dir) = fresh_store();
        save_page(&store, "temp", "Temporary", "# Temp\n\nWill delete.", &[]);
        assert!(dir.path().join(PAGES_DIR).join("temp.md").exists());
        store.get_pages().remove("temp").unwrap();
        assert!(!dir.path().join(PAGES_DIR).join("temp.md").exists());
        assert!(store.get_index().is_empty());
    }

    #[test]
    fn remove_page_errors_when_not_in_index() {
        let (store, _dir) = fresh_store();
        let err = store.get_pages().remove("nonexistent").unwrap_err();
        assert!(matches!(err, KnowledgeError::PageNotFound { .. }));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn clear_removes_all_pages_and_index() {
        let (store, dir) = fresh_store();
        save_page(&store, "a", "Page A", "# A", &[]);
        save_page(&store, "b", "Page B", "# B", &[]);
        store.clear().unwrap();
        assert!(store.get_index().is_empty());
        assert!(!dir.path().join(INDEX_FILE).exists());
        assert!(dir.path().join(PAGES_DIR).exists());
        let page_count = fs::read_dir(dir.path().join(PAGES_DIR)).unwrap().count();
        assert_eq!(page_count, 0);
    }

    #[test]
    fn normalize_slug_rejects_empty() {
        assert!(normalize_slug("").is_err());
    }

    #[test]
    fn normalize_slug_truncates_long_input() {
        let long = "a".repeat(80);
        let slug = normalize_slug(&long).unwrap();
        assert!(slug.len() <= 60);
    }

    #[test]
    fn normalize_slug_lowercases() {
        assert_eq!(normalize_slug("Deploy").unwrap(), "deploy");
        assert_eq!(normalize_slug("DEPLOY-Config").unwrap(), "deploy-config");
    }

    #[test]
    fn normalize_slug_strips_leading_trailing_hyphens() {
        assert_eq!(normalize_slug("-deploy").unwrap(), "deploy");
        assert_eq!(normalize_slug("deploy-").unwrap(), "deploy");
        assert_eq!(normalize_slug("--deploy--").unwrap(), "deploy");
    }

    #[test]
    fn normalize_slug_converts_spaces_and_underscores() {
        assert_eq!(normalize_slug("deploy config").unwrap(), "deploy-config");
        assert_eq!(normalize_slug("deploy_config").unwrap(), "deploy-config");
    }

    #[test]
    fn normalize_slug_converts_file_paths() {
        assert_eq!(
            normalize_slug("src/malware/bad_file.py").unwrap(),
            "src-malware-bad-file-py"
        );
    }

    #[test]
    fn normalize_slug_collapses_consecutive_hyphens() {
        assert_eq!(normalize_slug("a---b").unwrap(), "a-b");
        assert_eq!(normalize_slug("a/./b").unwrap(), "a-b");
    }

    #[test]
    fn normalize_slug_passes_through_valid() {
        assert_eq!(normalize_slug("deploy-config").unwrap(), "deploy-config");
        assert_eq!(normalize_slug("a").unwrap(), "a");
        assert_eq!(normalize_slug("config-v2").unwrap(), "config-v2");
        assert_eq!(normalize_slug("a1b2c3").unwrap(), "a1b2c3");
    }

    #[test]
    fn frontmatter_is_stripped_correctly() {
        let raw = "---\ntype: Knowledge\ntimestamp: 2026-01-01T00:00:00Z\ntags: [a, b]\n---\n# Title\n\nBody.";
        let body = strip_frontmatter(raw);
        assert_eq!(body, "# Title\n\nBody.");
    }

    #[test]
    fn strip_frontmatter_no_frontmatter() {
        let raw = "# Title\n\nBody.";
        assert_eq!(strip_frontmatter(raw), raw);
    }

    #[test]
    fn index_under_the_limit_lists_every_page() {
        let (store, _dir) = fresh_store();
        save_page(&store, "alpha", "First note", "# Alpha", &[]);
        save_page(&store, "beta", "Second note", "# Beta", &[]);

        let index = store.get_index();
        assert!(index.contains("alpha"), "{index}");
        assert!(index.contains("beta"), "{index}");
        assert!(!index.contains(INDEX_FILE), "{index}");
    }

    #[test]
    fn index_past_the_limit_lists_the_pages_that_fit_then_names_the_file() {
        let (store, dir) = fresh_store();
        // Room for the directive, which names an absolute path, plus a few pages.
        let store = store.set_index_char_limit(500);
        for i in 0..40 {
            save_page(store, &format!("page-{i:02}"), "A note", "# Note", &[]);
        }

        let index = store.get_index();
        assert!(index.contains("page-00"), "{index}");
        assert!(!index.contains("page-39"), "{index}");
        let listed = index.matches("- **").count();
        assert!(
            index.contains(&format!("{} more pages are not listed", 40 - listed)),
            "{index}"
        );
        let path = fs::canonicalize(dir.path()).unwrap().join(INDEX_FILE);
        assert!(index.contains(&path.display().to_string()), "{index}");
    }

    #[test]
    fn index_names_the_file_when_no_page_fits() {
        let (store, _dir) = fresh_store();
        let store = store.set_index_char_limit(10);
        save_page(store, "alpha", "First note", "# Alpha", &[]);

        let index = store.get_index();
        assert!(index.starts_with("1 more page is not listed"), "{index}");
    }

    #[test]
    fn a_write_past_the_limit_is_still_saved_and_listed_in_full() {
        let (store, _dir) = fresh_store();
        let store = store.set_index_char_limit(80);
        let long_description = "x".repeat(200);
        store
            .get_pages()
            .save(Page {
                slug: "big".into(),
                kind: String::new(),
                description: long_description,
                content: "# Big".into(),
                tags: vec![],
            })
            .unwrap();

        assert!(store.full_index().contains("big"));
        assert_eq!(
            store.get_pages().get_page("big").unwrap().content,
            "# Big\n"
        );

        save_page(store, "small", "ok", "# Small", &[]);
        let (_, limit, _) = store.index_usage();
        assert_eq!(limit, 80);
    }

    #[test]
    fn index_usage_reports_chars_limit_and_pages() {
        let (store, _dir) = fresh_store();
        save_page(&store, "test", "A test", "# Test\n\nContent.", &[]);
        let (used, limit, pages) = store.index_usage();
        assert!(used > 0);
        assert_eq!(limit, DEFAULT_INDEX_CHAR_LIMIT);
        assert_eq!(pages, 1);
    }

    #[test]
    fn writes_through_one_arc_clone_are_visible_through_another() {
        let (store, _dir) = fresh_store();
        let other = Arc::clone(&store);
        save_page(
            &store,
            "shared",
            "Shared note",
            "# Shared\n\nShared content.",
            &[],
        );
        assert!(other.get_index().contains("shared"));
    }

    #[test]
    fn entries_survive_drop_and_reopen() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let s1 = Knowledge::load(dir.path()).unwrap();
        save_page(
            &s1,
            "durable",
            "Survives restart",
            "# Durable\n\nPersisted.",
            &[],
        );
        drop(s1);
        let s2 = Knowledge::load(dir.path()).unwrap();
        assert!(s2.get_index().contains("durable"));
        assert!(s2.get_index().contains("Survives restart"));
    }

    #[test]
    fn rebuild_index_from_pages_when_index_missing() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let pages_dir = dir.path().join(PAGES_DIR);
        fs::create_dir_all(&pages_dir).unwrap();
        fs::write(
            pages_dir.join("my-page.md"),
            "---\nupdated: 2026-01-01T00:00:00Z\n---\n# My Page\n\nContent here.",
        )
        .unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let idx = store.get_index();
        assert!(idx.contains("my-page"));
        assert!(idx.contains("My Page"));
    }

    #[test]
    fn migration_from_memory_jsonl() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let legacy = dir.path().join(LEGACY_MEMORY_FILE);
        fs::write(
            &legacy,
            "{\"content\":\"fact one\",\"added_at\":1}\n\
             {\"content\":\"fact two\",\"added_at\":2}\n",
        )
        .unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let idx = store.get_index();
        assert!(idx.contains("legacy-notes"));
        assert!(idx.contains("2 entries"));
        assert!(!legacy.exists());
        assert!(dir
            .path()
            .join(format!("{LEGACY_MEMORY_FILE}{MIGRATED_SUFFIX}"))
            .exists());
        assert!(dir.path().join(PAGES_DIR).join("legacy-notes.md").exists());
        let body = store
            .get_pages()
            .get_page("legacy-notes")
            .map(|p| p.content)
            .unwrap();
        assert!(body.contains("fact one"));
        assert!(body.contains("fact two"));
    }

    #[test]
    fn save_page_with_tags() {
        let (store, dir) = fresh_store();
        save_page(
            &store,
            "tagged",
            "A tagged page",
            "# Tagged\n\nWith tags.",
            &["config", "deploy"],
        );
        let raw = fs::read_to_string(dir.path().join(PAGES_DIR).join("tagged.md")).unwrap();
        assert!(raw.contains("tags: [config, deploy]"));
    }

    #[test]
    fn write_page_rejects_empty_description() {
        let (store, _dir) = fresh_store();
        let err = store
            .get_pages()
            .save(Page {
                slug: "test".into(),
                kind: String::new(),
                description: String::new(),
                content: "content".into(),
                tags: vec![],
            })
            .unwrap_err();
        assert!(err.to_string().contains("Description"));
    }

    #[test]
    fn write_page_rejects_empty_content() {
        let (store, _dir) = fresh_store();
        let err = store
            .get_pages()
            .save(Page {
                slug: "test".into(),
                kind: String::new(),
                description: "a description".to_string(),
                content: "".into(),
                tags: vec![],
            })
            .unwrap_err();
        assert!(err.to_string().contains("Content"));
    }

    #[test]
    fn remove_page_empties_index_usage() {
        let (store, _dir) = fresh_store();
        save_page(&store, "temp", "Temp", "# Temp", &[]);
        store.get_pages().remove("temp").unwrap();
        let (used, _, pages) = store.index_usage();
        assert_eq!(used, 0);
        assert_eq!(pages, 0);
    }

    #[test]
    fn page_frontmatter_uses_okf_keys() {
        let (store, dir) = fresh_store();
        save_page(&store, "cfg", "Config page", "# Config\n\nBody.", &[]);
        let raw = fs::read_to_string(dir.path().join(PAGES_DIR).join("cfg.md")).unwrap();
        assert!(raw.contains("type: Knowledge"));
        assert!(raw.contains("description: Config page"));
        assert!(raw.contains("timestamp: "));
    }

    #[test]
    fn index_file_is_okf_progressive_disclosure() {
        let (store, dir) = fresh_store();
        save_page(&store, "cfg", "Config page", "# Config", &[]);
        let index = fs::read_to_string(dir.path().join(INDEX_FILE)).unwrap();
        assert!(index.contains("* [cfg](pages/cfg.md) - Config page"));
        assert!(!index.contains("---"));
    }

    #[test]
    fn agent_set_type_round_trips_and_defaults_to_knowledge() {
        let (store, _dir) = fresh_store();
        store
            .get_pages()
            .save(Page {
                slug: "verdict".into(),
                kind: "Verdict".into(),
                description: "A verdict".into(),
                content: "# Verdict".into(),
                tags: vec![],
            })
            .unwrap();
        assert_eq!(
            store.get_pages().get_page("verdict").unwrap().kind,
            "Verdict"
        );
        // An empty kind falls back to the default on write.
        save_page(&store, "note", "A note", "# Note", &[]);
        assert_eq!(
            store.get_pages().get_page("note").unwrap().kind,
            "Knowledge"
        );
    }

    #[test]
    fn loads_external_okf_bundle_flattening_nested_slugs() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let tables = dir.path().join("tables");
        fs::create_dir_all(&tables).unwrap();
        fs::write(
            tables.join("orders.md"),
            "---\ntype: Table\ndescription: One row per order.\n---\n# Orders\n\nBody.",
        )
        .unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let idx = store.get_index();
        assert!(idx.contains("tables-orders"));
        assert!(idx.contains("One row per order."));
        // The nested concept stays readable through its flattened slug.
        let page = store.get_pages().get_page("tables-orders").unwrap();
        assert_eq!(page.kind, "Table");
        assert!(page.content.contains("Body."));
    }

    #[test]
    fn seeded_page_without_type_loads_as_knowledge() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let pages = dir.path().join(PAGES_DIR);
        fs::create_dir_all(&pages).unwrap();
        fs::write(
            pages.join("bare.md"),
            "---\ndescription: No type here.\n---\n# Bare\n\nBody.",
        )
        .unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        assert_eq!(
            store.get_pages().get_page("bare").unwrap().kind,
            "Knowledge"
        );
    }
}
