//! The actions agents can take, and the registry an agent's tools live in.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::knowledge::Knowledge;
use crate::agents::tickets::{Run, TicketQueue};
use crate::event::{EventKind, RepairKind};
use crate::providers::types::ContentBlock;
use crate::providers::{ProviderResult, ToolDefinition};
use crate::schemas::Schema;

use super::error::ToolError;

/// The largest result one tool may return. Anything longer is written to
/// `<ticket-dir>/outputs/<tool_use_id>.txt` and replaced with a short stub.
/// Roughly 12 500 tokens.
const PER_TOOL_CAP: usize = 50_000;

/// The largest total one turn's tool results may reach. When several tools each
/// return an acceptable size, the largest are written out until the turn fits.
/// Roughly 50 000 tokens.
const PER_TURN_CAP: usize = 200_000;

/// How much of the original output the stub still shows. It ends at the last
/// newline in that window, or at a character boundary, so a multi-byte
/// character is never cut in half.
const PREVIEW_CHARS: usize = 2_000;

/// What a tool receives when it runs.
///
/// Writing your own tool, you need two of these:
/// - `dir`: the agent's working directory.
/// - `cancelled()`: resolves when the current call is given up on.
///
/// agentwerk fills in the rest for the built-in tools.
#[derive(Clone)]
pub struct ToolContext {
    /// Directory the tool runs in. Resolve a relative path against it.
    pub dir: PathBuf,
    pub(crate) run: Option<Arc<Run>>,
    pub(crate) ticket_queue: Option<Arc<TicketQueue>>,
    pub(crate) agent_id: Option<String>,
    pub(crate) ticket_key: Option<String>,
    pub(crate) knowledge: Option<Arc<Knowledge>>,
}

impl ToolContext {
    /// A context rooted at `dir` that is never cancelled. Use it standalone or
    /// in tests; agentwerk installs its own at call time.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            run: None,
            ticket_queue: None,
            agent_id: None,
            ticket_key: None,
            knowledge: None,
        }
    }

    pub(crate) fn run(mut self, run: Arc<Run>) -> Self {
        self.run = Some(run);
        self
    }

    pub(crate) fn ticket_queue(mut self, queue: Arc<TicketQueue>) -> Self {
        self.ticket_queue = Some(queue);
        self
    }

    pub(crate) fn agent_id(mut self, name: String) -> Self {
        self.agent_id = Some(name);
        self
    }

    pub(crate) fn ticket_key(mut self, key: String) -> Self {
        self.ticket_key = Some(key);
        self
    }

    pub(crate) fn knowledge(mut self, knowledge: Arc<Knowledge>) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// Publish `kind` for the ticket and agent this call runs for. A context
    /// with no queue publishes nothing; the call still runs.
    pub(crate) fn emit(&self, kind: EventKind) {
        let Some(queue) = &self.ticket_queue else {
            return;
        };
        let key = self.ticket_key.as_deref().unwrap_or_default();
        let agent = self.agent_id.as_deref().unwrap_or_default();
        queue.emit(key, agent, kind);
    }

    /// Resolves once the run starts to finish, whether the caller cancelled it
    /// or a limit was breached: either way this call is being given up on.
    ///
    /// Pair it with `tokio::select!` so the losing branch is dropped, which
    /// aborts an in-flight HTTP request and, with `kill_on_drop(true)`, ends a
    /// subprocess. On a standalone context it never resolves, and the `select!`
    /// behaves like a plain await.
    pub async fn cancelled(&self) {
        match &self.run {
            Some(run) => run.until_draining().await,
            None => std::future::pending::<()>().await,
        }
    }
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("dir", &self.dir)
            .field("has_ticket_queue", &self.ticket_queue.is_some())
            .finish()
    }
}

/// One tool call the model asked for.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Identifier for this call, sent back with the result.
    pub id: String,
    /// Name of the tool the model chose.
    pub name: String,
    /// Arguments the model supplied, matching the tool's input schema.
    pub input: Value,
}

/// What a tool reports back: a result, an error the model should work around,
/// or a rejection of its input.
///
/// All three reach the model the same way. The [`SchemaError`] variant is kept
/// apart so `max_schema_retries` can govern it.
///
/// [`SchemaError`]: ToolResult::SchemaError
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResult {
    /// The tool ran and produced this text.
    Success(String),
    /// The tool failed, and the message tells the model how to recover.
    Error(String),
    /// The tool rejected its input, and the message names what was wrong.
    SchemaError(String),
}

impl ToolResult {
    /// Report a result.
    pub fn success(content: impl Into<String>) -> Self {
        Self::Success(content.into())
    }

    /// Report a failure the model should work around.
    pub fn error(content: impl Into<String>) -> Self {
        Self::Error(content.into())
    }

    /// Report malformed input. It counts against `max_schema_retries`.
    pub fn schema_error(content: impl Into<String>) -> Self {
        Self::SchemaError(content.into())
    }

    /// Get the text, whichever outcome this is.
    pub fn content(&self) -> &str {
        let (Self::Success(content) | Self::Error(content) | Self::SchemaError(content)) = self;
        content
    }
}

/// What every tool implements.
///
/// Implement it on any type an agent should be able to call. For a tool defined
/// inline, use [`Tool`] and its builder instead.
pub trait ToolLike: Send + Sync {
    /// The name the model calls the tool by.
    fn name(&self) -> &str;

    /// What the tool does, in the words the model reads.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's arguments.
    fn input_schema(&self) -> Value;

    /// Whether the agent may run this tool in parallel with the turn's other
    /// concurrent calls; set it for a tool with no side effects. `false` by
    /// default.
    fn is_concurrent(&self) -> bool {
        false
    }

    /// The file paths this call opens, so they reach
    /// [`Stats`](crate::Stats). Empty by default.
    fn opened_paths(&self, _input: &Value) -> Vec<String> {
        Vec::new()
    }

    /// Run the tool.
    ///
    /// Cancellation drops the work in progress, so pair anything long-running
    /// with [`ToolContext::cancelled`] in a `tokio::select!`.
    fn call<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>>;
}

/// The tools one agent may call.
#[derive(Clone, Default)]
pub(crate) struct ToolRegistry {
    tools: Vec<Registered>,
}

/// A tool and the schema a call to it is checked against. Held together so
/// there is no tool the registry can reach without one.
#[derive(Clone)]
struct Registered {
    tool: Arc<dyn ToolLike>,
    schema: Schema,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.tools.iter().map(|r| r.tool.name()).collect();
        f.debug_struct("ToolRegistry")
            .field("tools", &names)
            .finish()
    }
}

impl ToolRegistry {
    /// Register a tool, replacing one already registered under that name.
    ///
    /// A request carrying one name twice is rejected, so the last registration
    /// is the one that counts.
    pub(crate) fn register(&mut self, tool: impl ToolLike + 'static) {
        let tool: Arc<dyn ToolLike> = Arc::new(tool);
        // A schema that does not compile is the author's mistake, not the
        // model's. Falling back to "must be an object" keeps the tool callable
        // and still checks the call, rather than leaving one tool unguarded.
        let schema = Schema::new(tool.input_schema()).unwrap_or_else(|_| object_schema());
        self.tools.retain(|r| r.tool.name() != tool.name());
        self.tools.push(Registered { tool, schema });
    }

    /// Get the arguments `name` advertised for `ticket`, which is what a call
    /// this registry rejected reads back.
    ///
    /// Resolved through the same spelling fold as [`get`](Self::get), so a folded
    /// name reads back the schema of the tool that would have run.
    pub(crate) fn advertised_schema(&self, name: &str, ticket: Option<&Schema>) -> Option<Value> {
        let tool = self.get(name)?;
        Some(advertised(&*tool, ticket))
    }

    /// Get the tool a call reaches and the schema it is checked against, owned,
    /// so a concurrent batch can move both into its task.
    ///
    /// The finish tool is checked against what it registered rather than what it
    /// advertised: it accepts a `result` envelope the advertised shape does not
    /// name, unwraps it, and validates the result against the ticket itself.
    fn resolve(&self, name: &str) -> Resolved {
        match self.find(name) {
            Some(found) => Ok((Arc::clone(&found.tool), found.schema.clone())),
            None => Err(ToolError::ToolNotFound {
                tool_name: name.into(),
                available: self.names(),
            }),
        }
    }

    /// Get the tool a call names.
    pub(crate) fn get(&self, name: &str) -> Option<Arc<dyn ToolLike>> {
        self.find(name).map(|found| Arc::clone(&found.tool))
    }

    /// Find what a call name reaches.
    ///
    /// An exact match wins. Otherwise a spelling that reduces to the same key as
    /// exactly one registered tool resolves to it, so a model that adds a
    /// `_tool` suffix still reaches the right tool.
    fn find(&self, name: &str) -> Option<&Registered> {
        let name = name.trim();
        if let Some(found) = self.tools.iter().find(|r| r.tool.name() == name) {
            return Some(found);
        }
        let key = lookup_key(name);
        let mut folded = self
            .tools
            .iter()
            .filter(|r| lookup_key(r.tool.name()) == key);
        let found = folded.next()?;
        // A key two tools share is ambiguous: refuse rather than guess.
        folded.next().is_none().then_some(found)
    }

    /// Get the registered names, sorted, for the error that tells the model what
    /// it could have called.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .iter()
            .map(|r| r.tool.name().to_string())
            .collect();
        names.sort();
        names
    }

    /// Get the tool definitions sent to the model, the finish tool's arguments
    /// carrying `ticket`'s schema.
    pub(crate) fn definitions(&self, ticket: Option<&Schema>) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|r| ToolDefinition {
                name: r.tool.name().to_string(),
                description: r.tool.description().to_string(),
                input_schema: advertised(&*r.tool, ticket),
            })
            .collect()
    }

    /// Run the calls, concurrent ones together and the rest one at a time.
    pub(crate) async fn execute(&self, calls: &[ToolCall], ctx: &ToolContext) -> Vec<ToolOutcome> {
        let batches = partition_tool_calls(calls, self);
        let mut results: Vec<ToolOutcome> = Vec::new();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(10));

        for batch in batches {
            match batch {
                ToolBatch::Concurrent(calls) => {
                    let mut set = tokio::task::JoinSet::new();
                    for call in calls {
                        let sem = semaphore.clone();
                        let ctx = ctx.clone();
                        // Resolved before the spawn: the task outlives this
                        // borrow of the registry.
                        let found = self.resolve(&call.name);
                        set.spawn(async move {
                            let _permit = sem.acquire().await.unwrap();
                            run_call(found, &call, &ctx).await
                        });
                    }

                    while let Some(join_result) = set.join_next().await {
                        if let Ok(outcome) = join_result {
                            results.push(outcome);
                        }
                    }
                }
                ToolBatch::Serial(call) => {
                    results.push(run_call(self.resolve(&call.name), &call, ctx).await);
                }
            }
        }

        cap_aggregate_outputs(&mut results, ctx, PER_TURN_CAP);

        results
    }
}

/// Compose the arguments `tool` shows the model for `ticket`. Only the finish
/// tool's depend on it; every other tool shows what it registered.
///
/// One answer serves the definitions the model is shown and the shape a
/// rejected call reads back, so the two cannot disagree.
fn advertised(tool: &dyn ToolLike, ticket: Option<&Schema>) -> Value {
    match ticket.filter(|_| tool.name() == super::TICKET_FINISH_TOOL) {
        Some(ticket) => super::finish_tool_input_schema(tool.input_schema(), Some(ticket)),
        None => tool.input_schema(),
    }
}

/// The schema a tool whose own document does not compile is checked against.
/// Every tool call carries an arguments object, so this is the least the
/// registry can hold a call to.
fn object_schema() -> Schema {
    Schema::new(serde_json::json!({"type": "object"})).expect("a literal object schema compiles")
}

/// Reduce a tool name to the key lookups match on, so capitalization, hyphens,
/// or a trailing `_tool` still reach the tool.
///
/// The reduction only removes information, so it cannot invent a name nobody
/// asked for. Where two tools share a key, [`ToolRegistry::get`] refuses.
fn lookup_key(name: &str) -> String {
    let key = name.trim().to_lowercase().replace('-', "_");
    match key.strip_suffix("_tool") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => key,
    }
}

type ToolHandler = Box<
    dyn Fn(
            Value,
            &ToolContext,
        ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + '_>>
        + Send
        + Sync,
>;

/// Collects what a [`Tool`] needs. `.build()` exists only once a handler is
/// installed, so a tool that does nothing cannot reach an agent.
pub struct ToolBuilder<H> {
    name: String,
    description: String,
    schema: Value,
    concurrent: bool,
    paths: Vec<String>,
    handler: H,
}

/// A `Tool` you define inline, for when a whole type would be too much. With
/// state to carry, implement [`ToolLike`] on your own type instead.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::{Tool, ToolResult};
/// use serde_json::json;
///
/// let greet = Tool::new("greet", "Say hello to a name.")
///     .schema(json!({
///         "type": "object",
///         "properties": { "name": { "type": "string" } },
///         "required": ["name"]
///     }))
///     .concurrent(true)
///     .handler(|input, _ctx| async move {
///         let name = input["name"].as_str().unwrap_or("world");
///         Ok(ToolResult::success(format!("Hello, {name}!")))
///     })
///     .build();
///
/// Agent::new().tool(greet);
/// ```
pub struct Tool {
    name: String,
    description: String,
    schema: Value,
    concurrent: bool,
    paths: Vec<String>,
    handler: ToolHandler,
}

impl Tool {
    /// Start building a tool. Add what it accepts with `.schema(...)`, what it
    /// does with `.handler(...)`, and finish with `.build()`.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> ToolBuilder<()> {
        ToolBuilder {
            name: name.into(),
            description: description.into(),
            schema: serde_json::json!({"type": "object", "properties": {}}),
            concurrent: false,
            paths: Vec::new(),
            handler: (),
        }
    }

    /// Start building a tool from a `.tool.md` definition, which supplies its
    /// name, description, input schema, and whether it may run concurrently.
    /// Panics when the file is malformed.
    pub fn from_tool_file(definition: &str) -> ToolBuilder<()> {
        let tf = super::tool_file::ToolFile::parse(definition);
        Tool::new(tf.name.clone(), tf.render_markdown())
            .schema(tf.input_schema.clone())
            .concurrent(tf.concurrent)
    }
}

impl<H> ToolBuilder<H> {
    /// Define what the tool accepts, as JSON Schema.
    pub fn schema(mut self, schema: Value) -> Self {
        self.schema = schema;
        self
    }

    /// Run this tool in parallel with the turn's other concurrent calls. Set it
    /// for a tool with no side effects.
    pub fn concurrent(mut self, concurrent: bool) -> Self {
        self.concurrent = concurrent;
        self
    }

    /// Name the input fields holding a file path, so the files a call opens are
    /// included in statistics. A field that is absent or not a string is
    /// skipped.
    pub fn paths<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.paths = fields.into_iter().map(Into::into).collect();
        self
    }
}

impl ToolBuilder<()> {
    /// Define what runs when the model calls this tool. A bare `async` block
    /// works.
    pub fn handler<F, Fut>(self, f: F) -> ToolBuilder<ToolHandler>
    where
        F: Fn(Value, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ProviderResult<ToolResult>> + Send + 'static,
    {
        ToolBuilder {
            name: self.name,
            description: self.description,
            schema: self.schema,
            concurrent: self.concurrent,
            paths: self.paths,
            handler: Box::new(move |v, c| Box::pin(f(v, c.clone()))),
        }
    }
}

impl ToolBuilder<ToolHandler> {
    /// Create the tool, ready for `Agent::tool(...)`.
    pub fn build(self) -> Tool {
        Tool {
            name: self.name,
            description: self.description,
            schema: self.schema,
            concurrent: self.concurrent,
            paths: self.paths,
            handler: self.handler,
        }
    }
}

impl ToolLike for Tool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    fn is_concurrent(&self) -> bool {
        self.concurrent
    }

    fn opened_paths(&self, input: &Value) -> Vec<String> {
        self.paths
            .iter()
            .filter_map(|field| input.get(field).and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect()
    }

    fn call<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
        (self.handler)(input, ctx)
    }
}

/// What one call produced: the block the model sees, the typed outcome, and the
/// file an oversized output was written to.
type ToolOutcome = (
    ContentBlock,
    std::result::Result<String, ToolError>,
    Option<PathBuf>,
);

/// What a call name resolved to: the tool and the schema its arguments are
/// checked against, or the failure naming what could have been called instead.
/// The tools a batch runs concurrently are resolved before the batch spawns, so
/// each task owns what it needs.
type Resolved = std::result::Result<(Arc<dyn ToolLike>, Schema), ToolError>;

/// Run one call to the outcome the turn collects: check the arguments, run the
/// tool, then stand in for an empty or oversized result.
async fn run_call(found: Resolved, call: &ToolCall, ctx: &ToolContext) -> ToolOutcome {
    let outcome = invoke(found, &call.name, call.input.clone(), ctx).await;
    let outcome = replace_empty_output(outcome, &call.name);
    let (outcome, path) = cap_oversized_result(outcome, ctx, &call.id, PER_TOOL_CAP);
    let block = content_block_for(&call.id, &outcome);
    (block, outcome, path)
}

/// Check the arguments against the schema the tool registered, then run it on
/// what survives.
async fn invoke(
    resolved: Resolved,
    name: &str,
    input: Value,
    ctx: &ToolContext,
) -> std::result::Result<String, ToolError> {
    let (tool, schema) = resolved?;
    // Retyped rather than refused, so a quoted number runs the call the model
    // asked for and arguments it wrote as JSON text are decoded. What comes
    // back names the value that produced, which is the one the tool would have
    // received.
    let (input, repairs) =
        schema
            .validate(input)
            .map_err(|violations| ToolError::SchemaValidationFailed {
                tool_name: name.into(),
                message: violations.to_string(),
            })?;
    for pointer in &repairs {
        ctx.emit(EventKind::ResponseRepaired {
            reason: RepairKind::ValueMistyped,
            message: repair_message(name, pointer),
        });
    }
    match tool.call(input, ctx).await {
        Ok(ToolResult::Success(s)) => Ok(s),
        Ok(ToolResult::Error(s)) => Err(ToolError::ExecutionFailed {
            tool_name: name.into(),
            message: s,
        }),
        Ok(ToolResult::SchemaError(s)) => Err(ToolError::SchemaValidationFailed {
            tool_name: name.into(),
            message: s,
        }),
        Err(e) => Err(ToolError::ExecutionFailed {
            tool_name: name.into(),
            message: e.to_string(),
        }),
    }
}

/// Name a repair `tool` made before running, by the JSON pointer it happened
/// at; the empty pointer is the value as a whole. One wording for every
/// reporter, since a host groups these by the tool name they lead with.
///
/// One verb covers both rewrites: decoding a payload the model wrote as text is
/// the whole-value case of retyping it.
pub(crate) fn repair_message(tool: &str, pointer: &str) -> String {
    match pointer {
        "" => format!("{tool}: retyped"),
        path => format!("{tool}: {path} retyped"),
    }
}

/// Put a placeholder in place of an empty result, since empty content has upset
/// LLM providers. Everything else passes through.
fn replace_empty_output(
    outcome: std::result::Result<String, ToolError>,
    tool_name: &str,
) -> std::result::Result<String, ToolError> {
    match outcome {
        Ok(s) if s.is_empty() => Ok(format!("({tool_name} completed with no output)")),
        other => other,
    }
}

fn content_block_for(
    tool_use_id: &str,
    outcome: &std::result::Result<String, ToolError>,
) -> ContentBlock {
    let (content, succeeded) = match outcome {
        Ok(s) => (s.clone(), true),
        Err(e) => (e.message(), false),
    };
    ContentBlock::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content,
        succeeded,
    }
}

enum ToolBatch {
    Concurrent(Vec<ToolCall>),
    Serial(ToolCall),
}

fn partition_tool_calls(calls: &[ToolCall], registry: &ToolRegistry) -> Vec<ToolBatch> {
    let mut batches: Vec<ToolBatch> = Vec::new();
    let mut concurrent_batch: Vec<ToolCall> = Vec::new();

    for call in calls {
        let is_concurrent = registry.get(&call.name).is_some_and(|t| t.is_concurrent());

        if is_concurrent {
            concurrent_batch.push(call.clone());
        } else {
            if !concurrent_batch.is_empty() {
                batches.push(ToolBatch::Concurrent(std::mem::take(&mut concurrent_batch)));
            }
            batches.push(ToolBatch::Serial(call.clone()));
        }
    }

    if !concurrent_batch.is_empty() {
        batches.push(ToolBatch::Concurrent(concurrent_batch));
    }

    batches
}

/// Replace an oversized result with a stub, writing the original under the
/// ticket's outputs directory and reporting where it went.
///
/// An error passes through, being short by construction, and so does the raw
/// content when the write fails.
fn cap_oversized_result(
    outcome: std::result::Result<String, ToolError>,
    ctx: &ToolContext,
    tool_use_id: &str,
    per_tool_cap: usize,
) -> (std::result::Result<String, ToolError>, Option<PathBuf>) {
    match outcome {
        Err(e) => (Err(e), None),
        Ok(content) if content.len() <= per_tool_cap => (Ok(content), None),
        Ok(content) => match persist_output(ctx, tool_use_id, &content) {
            None => (Ok(content), None),
            Some(p) => {
                let preview = truncate_preview(&content);
                let stub = format_oversized_tool_result(content.len(), &p.display, preview);
                (Ok(stub), Some(p.rel))
            }
        },
    }
}

/// While one turn's results are too large together, write out the largest that
/// is not already a stub. It stops once the turn fits, or once nothing left can
/// be written out.
fn cap_aggregate_outputs(results: &mut [ToolOutcome], ctx: &ToolContext, per_turn_cap: usize) {
    loop {
        let total: usize = results
            .iter()
            .map(|(b, _, _)| match b {
                ContentBlock::ToolResult { content, .. } => content.len(),
                _ => 0,
            })
            .sum();
        if total <= per_turn_cap {
            return;
        }
        let mut largest: Option<(usize, usize)> = None;
        for (i, (b, _, _)) in results.iter().enumerate() {
            if let ContentBlock::ToolResult { content, .. } = b {
                if content.starts_with(OVERSIZED_STUB_TAG_OPEN) {
                    continue;
                }
                let len = content.len();
                if largest.is_none_or(|(_, max_len)| len > max_len) {
                    largest = Some((i, len));
                }
            }
        }
        let Some((i, _)) = largest else {
            return;
        };
        let ContentBlock::ToolResult {
            tool_use_id,
            content,
            succeeded,
        } = &results[i].0
        else {
            return;
        };
        let tool_use_id = tool_use_id.clone();
        let original = content.clone();
        let succeeded = *succeeded;
        let Some(p) = persist_output(ctx, &tool_use_id, &original) else {
            // Persistence failed; nothing further this pass can do.
            return;
        };
        let preview = truncate_preview(&original);
        let stub = format_oversized_tool_result(original.len(), &p.display, preview);
        results[i].0 = ContentBlock::ToolResult {
            tool_use_id,
            content: stub.clone(),
            succeeded,
        };
        if results[i].1.is_ok() {
            results[i].1 = Ok(stub);
        }
        results[i].2 = Some(p.rel);
    }
}

/// Write `content` under the ticket's outputs directory, reporting both the
/// path relative to the session and the path on disk.
///
/// `None` when the context names no ticket, no ticket queue is attached, or
/// the write fails. Like the rest of the logging, it is best effort.
fn persist_output(ctx: &ToolContext, tool_use_id: &str, content: &str) -> Option<PersistedOutput> {
    let queue = ctx.ticket_queue.as_ref()?;
    let key = ctx.ticket_key.as_deref()?;
    let rel = queue.write_tool_output(key, tool_use_id, content)?;
    let display = queue.get_dir().join(&rel);
    Some(PersistedOutput { rel, display })
}

struct PersistedOutput {
    rel: PathBuf,
    display: PathBuf,
}

const OVERSIZED_STUB_TAG_OPEN: &str = "<persisted-output>";
const OVERSIZED_STUB_TAG_CLOSE: &str = "</persisted-output>";

/// Build the stub the model sees in place of an oversized result: how large it
/// was, where it went, and how it starts.
fn format_oversized_tool_result(original_len: usize, path: &Path, preview: &str) -> String {
    let size = format_bytes(original_len);
    let preview_size = format_bytes(preview.len());
    format!(
        "{OVERSIZED_STUB_TAG_OPEN}Output too large ({size}). Full output saved to: {path}\n\
         Preview (first {preview_size}):\n\
         {preview}\n\
         {OVERSIZED_STUB_TAG_CLOSE}",
        path = path.display(),
    )
}

/// The first `PREVIEW_CHARS` bytes of `content`, ending at the last newline in
/// that window or at a character boundary.
///
/// The window is moved to a character boundary first: `PREVIEW_CHARS` can land
/// inside a multi-byte character, and slicing there would panic.
fn truncate_preview(content: &str) -> &str {
    let window = utf8_boundary_floor(content, PREVIEW_CHARS.min(content.len()));
    let cut = content[..window]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(window);
    &content[..cut]
}

fn format_bytes(n: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if n < 1024 {
        format!("{n} B")
    } else if (n as f64) < MB {
        format!("{:.1} KB", n as f64 / KB)
    } else {
        format!("{:.1} MB", n as f64 / MB)
    }
}

/// Move an index back to the nearest character boundary, at most three bytes.
fn utf8_boundary_floor(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool agentwerk registers, for the checks that hold across all of
    /// them. The knowledge store is temporary; its tool is read, not used.
    fn built_in_tools() -> (Vec<Box<dyn ToolLike>>, crate::test_util::TempDir) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = crate::agents::knowledge::Knowledge::load(dir.path()).unwrap();
        let tools: Vec<Box<dyn ToolLike>> = vec![
            Box::new(crate::tools::ReadFileTool),
            Box::new(crate::tools::WriteFileTool),
            Box::new(crate::tools::EditFileTool),
            Box::new(crate::tools::GlobTool),
            Box::new(crate::tools::GrepTool),
            Box::new(crate::tools::ListDirectoryTool),
            Box::new(crate::tools::FetchUrlTool),
            Box::new(crate::tools::KnowledgeTool::new(store)),
            Box::new(crate::tools::CommandTool::new("git").allow("git *")),
            Box::new(crate::tools::FinishTool),
            Box::new(crate::tools::TicketsTool),
        ];
        (tools, dir)
    }

    /// Every built-in `.tool.md` must parse and compile. Reading each tool's
    /// name, description, and schema forces that here, rather than at the first
    /// call.
    #[test]
    fn every_built_in_tool_definition_parses_and_compiles() {
        let (tools, _dir) = built_in_tools();
        for tool in &tools {
            assert!(!tool.name().is_empty(), "tool name is empty");
            assert!(
                !tool.description().is_empty(),
                "empty description for {}",
                tool.name(),
            );
            let document = tool.input_schema();
            // The registry holds a call to this, so a tool that declares
            // something else loses the check that its arguments are an object.
            assert_eq!(
                document["type"],
                "object",
                "arguments are not an object for {}",
                tool.name(),
            );
            assert!(
                Schema::new(document).is_ok(),
                "schema did not compile for {}",
                tool.name(),
            );
        }
    }

    #[test]
    fn every_example_a_built_in_tool_shows_is_a_call_its_own_schema_accepts() {
        // An example needing a repair, or failing outright, teaches the model a
        // shape it will be corrected for.
        let (tools, _dir) = built_in_tools();
        for tool in &tools {
            let document = tool.input_schema();
            let schema = Schema::new(document.clone()).expect("schema compiles");
            let examples = document["examples"]
                .as_array()
                .unwrap_or_else(|| panic!("{} shows no examples", tool.name()))
                .clone();
            for example in examples {
                let (_, repaired) = schema
                    .validate(example.clone())
                    .unwrap_or_else(|violations| panic!("{}: {violations}", tool.name()));
                assert!(repaired.is_empty(), "{} repaired {example}", tool.name());
            }
        }
    }

    #[test]
    fn registering_a_name_twice_leaves_the_later_tool() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("echo", true, "first"));
        registry.register(MockTool::new("echo", true, "second"));

        let definitions = registry.definitions(None);
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name, "echo");
    }

    #[test]
    fn paths_reports_the_named_input_fields() {
        let tool = Tool::new("cat", "Read a file.")
            .paths(["path", "into"])
            .handler(|_input, _ctx| async move { Ok(ToolResult::success("ok")) })
            .build();

        let input = serde_json::json!({"path": "src/lib.rs", "limit": 20});
        assert_eq!(tool.opened_paths(&input), vec!["src/lib.rs".to_string()]);
    }

    /// The mock the registry tests share.
    struct MockTool {
        name: String,
        concurrent: bool,
        result: String,
    }

    impl MockTool {
        fn new(name: &str, concurrent: bool, result: &str) -> Self {
            Self {
                name: name.into(),
                concurrent,
                result: result.into(),
            }
        }
    }

    impl ToolLike for MockTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "mock"
        }
        fn input_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        fn is_concurrent(&self) -> bool {
            self.concurrent
        }
        fn call<'a>(
            &'a self,
            _input: Value,
            _ctx: &'a ToolContext,
        ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
            let result = self.result.clone();
            Box::pin(async move { Ok(ToolResult::success(result)) })
        }
    }

    fn test_ctx() -> ToolContext {
        ToolContext::new(std::env::current_dir().unwrap())
    }

    #[test]
    fn registry_register_and_get() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("read_file", true, "file contents"));
        assert!(registry.get("read_file").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn resolves_a_name_carrying_a_tool_suffix() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("grep", true, "matches"));

        let tool = registry.get("grep_tool").expect("suffix should fold away");
        assert_eq!(tool.name(), "grep");
    }

    #[test]
    fn resolves_a_name_the_model_hyphenated() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("read_file", true, "file contents"));

        let tool = registry
            .get("Read-File")
            .expect("case and hyphen should fold");
        assert_eq!(tool.name(), "read_file");
    }

    #[test]
    fn refuses_a_name_two_tools_share_a_key() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("grep", true, "builtin"));
        registry.register(MockTool::new("grep_tool", true, "host tool"));

        assert!(registry.get("Grep").is_none());
        // Each registered name still reaches its own tool: exact match wins.
        assert_eq!(registry.get("grep").unwrap().name(), "grep");
        assert_eq!(registry.get("grep_tool").unwrap().name(), "grep_tool");
    }

    #[test]
    fn from_tool_file_populates_name_description_schema_concurrent() {
        let definition = r#"---
name: demo_tool
concurrent: true
---

Do the demo thing.

- Returns nothing useful.

## Schema

```json
{
  "type": "object",
  "properties": {"x": {"type": "string"}},
  "required": ["x"]
}
```
"#;
        let tool = Tool::from_tool_file(definition)
            .handler(|_, _| async { Ok(ToolResult::success("")) })
            .build();
        assert_eq!(tool.name(), "demo_tool");
        assert!(tool.description().contains("Do the demo thing."));
        assert!(tool.description().contains("- Returns nothing useful."));
        assert!(tool.is_concurrent());
        let schema = tool.input_schema();
        assert_eq!(schema["properties"]["x"]["type"], "string");
        assert_eq!(schema["required"][0], "x");
    }

    #[test]
    fn registry_definitions() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("read", true, "ok"));
        registry.register(MockTool::new("write", false, "ok"));

        let defs = registry.definitions(None);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "read");
        assert_eq!(defs[1].name, "write");
    }

    #[test]
    fn a_rejected_call_reads_back_the_arguments_its_tool_advertised() {
        let mut registry = ToolRegistry::default();
        registry.register(crate::tools::FinishTool);
        let ticket = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"partial_sum": {"type": "integer"}},
            "required": ["partial_sum"],
        }))
        .unwrap();

        let definitions = registry.definitions(Some(&ticket));
        let advertised = registry.advertised_schema("finish", Some(&ticket)).unwrap();

        let shown = definitions
            .iter()
            .find(|definition| definition.name == "finish")
            .expect("finish is registered");
        assert_eq!(shown.input_schema, advertised);
        assert!(
            advertised["properties"]["partial_sum"].is_object(),
            "{advertised}"
        );
    }

    #[test]
    fn a_tool_the_ticket_says_nothing_about_advertises_what_it_registered() {
        let mut registry = ToolRegistry::default();
        registry.register(TypedTool);
        let ticket = Schema::new(serde_json::json!({"type": "string"})).unwrap();

        let advertised = registry.advertised_schema("typed", Some(&ticket)).unwrap();

        assert_eq!(advertised, TypedTool.input_schema());
    }

    #[test]
    fn registry_clone() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("t", true, "ok"));
        let cloned = registry.clone();
        assert_eq!(cloned.definitions(None).len(), 1);
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let registry = ToolRegistry::default();
        let ctx = test_ctx();
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "nonexistent".into(),
            input: serde_json::json!({}),
        }];

        let results = registry.execute(&calls, &ctx).await;
        assert_eq!(results.len(), 1);
        match &results[0].0 {
            ContentBlock::ToolResult {
                succeeded, content, ..
            } => {
                assert!(!succeeded);
                assert!(content.contains("Unknown tool"));
            }
            other => panic!("Expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_object_input_is_reported_as_such_and_never_reaches_the_tool() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("grep", true, "matches"));
        let ctx = test_ctx();
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "grep".into(),
            input: Value::String(r#"{"pattern": "exec""#.into()),
        }];

        let results = registry.execute(&calls, &ctx).await;
        let ContentBlock::ToolResult {
            content, succeeded, ..
        } = &results[0].0
        else {
            panic!("Expected ToolResult");
        };
        assert!(!succeeded);
        assert!(
            content.contains("expected type object, got string"),
            "{content}"
        );
        assert!(
            content.contains("send it as JSON, not as a string"),
            "{content}"
        );
        assert_ne!(content, "matches");
    }

    /// A tool that declares what its one argument must be, so dispatch has
    /// something to check a call against. It answers with the argument it
    /// received, which is what the retype tests read.
    struct TypedTool;

    impl ToolLike for TypedTool {
        fn name(&self) -> &str {
            "typed"
        }
        fn description(&self) -> &str {
            "typed"
        }
        fn input_schema(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": { "count": { "type": "integer" } },
                "required": ["count"],
            })
        }
        fn call<'a>(
            &'a self,
            input: Value,
            _ctx: &'a ToolContext,
        ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
            Box::pin(async move { Ok(ToolResult::success(input["count"].to_string())) })
        }
    }

    async fn call_typed(input: Value) -> (String, bool) {
        call_typed_in(input, &test_ctx()).await
    }

    async fn call_typed_in(input: Value, ctx: &ToolContext) -> (String, bool) {
        let mut registry = ToolRegistry::default();
        registry.register(TypedTool);
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "typed".into(),
            input,
        }];
        let results = registry.execute(&calls, ctx).await;
        let ContentBlock::ToolResult {
            content, succeeded, ..
        } = &results[0].0
        else {
            panic!("Expected ToolResult");
        };
        (content.clone(), *succeeded)
    }

    #[tokio::test]
    async fn arguments_matching_the_declared_schema_reach_the_tool() {
        let (content, succeeded) = call_typed(serde_json::json!({"count": 3})).await;
        assert!(succeeded);
        assert_eq!(content, "3");
    }

    #[tokio::test]
    async fn an_argument_the_model_quoted_is_retyped_before_the_tool_runs() {
        let (content, succeeded) = call_typed(serde_json::json!({"count": "3"})).await;
        assert!(succeeded);
        assert_eq!(content, "3");
    }

    #[tokio::test]
    async fn arguments_the_model_quoted_whole_are_decoded_before_the_tool_runs() {
        let (content, succeeded) = call_typed(Value::String(r#"{"count": 3}"#.into())).await;
        assert!(succeeded);
        assert_eq!(content, "3");
    }

    #[tokio::test]
    async fn an_argument_no_retype_recovers_is_named_and_never_reaches_the_tool() {
        let (content, succeeded) = call_typed(serde_json::json!({"count": "three"})).await;
        assert!(!succeeded);
        assert!(content.contains("count"), "{content}");
    }

    #[tokio::test]
    async fn a_missing_required_argument_is_named_and_never_reaches_the_tool() {
        let (content, succeeded) = call_typed(serde_json::json!({})).await;
        assert!(!succeeded);
        assert!(content.contains("count"), "{content}");
    }

    #[tokio::test]
    async fn a_retyped_argument_is_reported_as_a_repair() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        let seen: Arc<std::sync::Mutex<Vec<(RepairKind, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = Arc::clone(&seen);
        queue.on_event(move |event| {
            if let EventKind::ResponseRepaired { reason, message } = &event.kind {
                collected.lock().unwrap().push((*reason, message.clone()));
            }
        });
        let ctx = ToolContext::new(dir.path().to_path_buf())
            .ticket_queue(Arc::clone(&queue))
            .agent_id("alice".into())
            .ticket_key("TICKET-1".into());

        let (_, succeeded) = call_typed_in(serde_json::json!({"count": "3"}), &ctx).await;

        assert!(succeeded);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(
                RepairKind::ValueMistyped,
                "typed: /count retyped".to_string()
            )]
        );

        seen.lock().unwrap().clear();
        // No pointer names a whole payload the model wrote as text.
        let arguments = Value::String(r#"{"count": 3}"#.into());
        let (_, succeeded) = call_typed_in(arguments, &ctx).await;

        assert!(succeeded);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(RepairKind::ValueMistyped, "typed: retyped".to_string())]
        );
    }

    #[tokio::test]
    async fn an_argument_the_schema_does_not_declare_still_reaches_the_tool() {
        // Tool schemas close over nothing: an extra key is generosity, not error.
        let (_, succeeded) = call_typed(serde_json::json!({"count": 3, "extra": true})).await;
        assert!(succeeded);
    }

    #[tokio::test]
    async fn a_tool_whose_schema_does_not_compile_is_still_held_to_an_object() {
        // The mistake is the author's; the agent should not pay for it with an
        // uncallable tool, nor with the one call nothing checks.
        struct Unschemable;
        impl ToolLike for Unschemable {
            fn name(&self) -> &str {
                "unschemable"
            }
            fn description(&self) -> &str {
                "unschemable"
            }
            fn input_schema(&self) -> Value {
                serde_json::json!({"uniqueItems": true})
            }
            fn call<'a>(
                &'a self,
                _input: Value,
                _ctx: &'a ToolContext,
            ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
                Box::pin(async move { Ok(ToolResult::success("ran")) })
            }
        }
        let mut registry = ToolRegistry::default();
        registry.register(Unschemable);
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "unschemable".into(),
            input: serde_json::json!({"anything": 1}),
        }];
        let results = registry.execute(&calls, &test_ctx()).await;
        let ContentBlock::ToolResult { content, .. } = &results[0].0 else {
            panic!("Expected ToolResult");
        };
        assert_eq!(content, "ran");

        let calls = vec![ToolCall {
            id: "c2".into(),
            name: "unschemable".into(),
            input: Value::String("not an object".into()),
        }];
        let results = registry.execute(&calls, &test_ctx()).await;
        let ContentBlock::ToolResult {
            content, succeeded, ..
        } = &results[0].0
        else {
            panic!("Expected ToolResult");
        };
        assert!(!succeeded);
        assert!(content.contains("expected type object"), "{content}");
    }

    #[tokio::test]
    async fn unknown_tool_error_names_the_registered_tools() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("grep", true, "matches"));
        registry.register(MockTool::new("read_file", true, "file contents"));
        let ctx = test_ctx();
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "ripgrep".into(),
            input: serde_json::json!({}),
        }];

        let results = registry.execute(&calls, &ctx).await;
        let ContentBlock::ToolResult { content, .. } = &results[0].0 else {
            panic!("Expected ToolResult");
        };
        assert_eq!(
            content,
            "Unknown tool: ripgrep. Available tools: grep, read_file"
        );
    }

    #[tokio::test]
    async fn execute_concurrent_tools_at_the_same_time() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("read1", true, "result1"));
        registry.register(MockTool::new("read2", true, "result2"));
        let ctx = test_ctx();

        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "read1".into(),
                input: serde_json::json!({}),
            },
            ToolCall {
                id: "c2".into(),
                name: "read2".into(),
                input: serde_json::json!({}),
            },
        ];

        let results = registry.execute(&calls, &ctx).await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn execute_serial_tool() {
        let mut registry = ToolRegistry::default();
        registry.register(MockTool::new("write_file", false, "written"));
        let ctx = test_ctx();

        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            input: serde_json::json!({"path": "/tmp/test"}),
        }];

        let results = registry.execute(&calls, &ctx).await;
        assert_eq!(results.len(), 1);
        match &results[0].0 {
            ContentBlock::ToolResult {
                content, succeeded, ..
            } => {
                assert!(succeeded);
                assert_eq!(content, "written");
            }
            other => panic!("Expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn tool_basic() {
        let tool = Tool::new("echo", "Echoes input")
            .schema(
                serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            )
            .concurrent(true)
            .handler(|input, _ctx| async move {
                let text = input["text"].as_str().unwrap_or("").to_string();
                Ok(ToolResult::success(text))
            })
            .build();

        assert_eq!(tool.name(), "echo");
        assert!(tool.is_concurrent());
    }

    // Layer 1: result-cap helpers

    fn ticket_ctx() -> (
        ToolContext,
        Arc<TicketQueue>,
        String,
        crate::test_util::TempDir,
    ) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        queue.task("seed");
        let key = "TICKET-1".to_string();
        let ctx = test_ctx()
            .ticket_queue(Arc::clone(&queue))
            .ticket_key(key.clone());
        (ctx, queue, key, dir)
    }

    fn tool_result_block(id: &str, content: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_use_id: id.into(),
            content: content.into(),
            succeeded: true,
        }
    }

    fn relative_outputs_path(key: &str, tool_use_id: &str) -> PathBuf {
        PathBuf::from("tickets")
            .join(key)
            .join("outputs")
            .join(format!("{tool_use_id}.txt"))
    }

    fn absolute_outputs_path(dir: &std::path::Path, key: &str, tool_use_id: &str) -> PathBuf {
        dir.join(relative_outputs_path(key, tool_use_id))
    }

    #[test]
    fn write_tool_output_stores_relative_path_in_comment() {
        let (ctx, _queue, key, _dir) = ticket_ctx();
        let (_outcome, path) = cap_oversized_result(Ok("z".repeat(500)), &ctx, "call-rel", 100);
        let stored = path.expect("offload happened");
        assert_eq!(stored, relative_outputs_path(&key, "call-rel"));
        assert!(
            stored.is_relative(),
            "comment path must stay portable: {}",
            stored.display()
        );
    }

    #[test]
    fn persisted_output_renders_absolute_path_for_model() {
        let (ctx, _queue, key, dir) = ticket_ctx();
        let (outcome, _path) = cap_oversized_result(Ok("y".repeat(500)), &ctx, "call-abs", 100);
        let stub = outcome.unwrap();
        let absolute = absolute_outputs_path(dir.path(), &key, "call-abs");
        assert!(
            stub.contains(&absolute.display().to_string()),
            "stub must give the model the joinable on-disk path: {stub}"
        );
    }

    #[test]
    fn cap_oversized_result_passes_through_under_cap() {
        let ctx = test_ctx();
        let (outcome, path) = cap_oversized_result(Ok("hello".into()), &ctx, "call-1", 100);
        assert_eq!(outcome.unwrap(), "hello");
        assert!(path.is_none());
    }

    #[test]
    fn cap_oversized_result_replaces_oversized_ok_with_stub() {
        let (ctx, _queue, key, dir) = ticket_ctx();
        let (outcome, path) = cap_oversized_result(Ok("a".repeat(500)), &ctx, "call-xyz", 100);
        let stub = outcome.unwrap();
        assert!(stub.starts_with("<persisted-output>"));
        assert!(stub.contains("Output too large"));
        assert!(stub.contains("Full output saved to:"));
        let absolute = absolute_outputs_path(dir.path(), &key, "call-xyz");
        assert!(
            stub.contains(&absolute.display().to_string()),
            "stub must name the absolute path so the model can read the file: {stub}"
        );
        assert!(stub.contains("Preview (first"));
        assert!(stub.ends_with("</persisted-output>"));
        let path = path.expect("offload path");
        assert_eq!(path, relative_outputs_path(&key, "call-xyz"));
        let body = std::fs::read_to_string(&absolute).unwrap();
        assert_eq!(body, "a".repeat(500));
    }

    #[test]
    fn cap_oversized_result_passes_through_errs() {
        let ctx = test_ctx();
        let (outcome, path) = cap_oversized_result(
            Err(ToolError::ExecutionFailed {
                tool_name: "tool".into(),
                message: "boom".into(),
            }),
            &ctx,
            "call-1",
            10,
        );
        assert!(matches!(outcome, Err(ToolError::ExecutionFailed { .. })));
        assert!(path.is_none());
    }

    #[test]
    fn cap_oversized_result_returns_raw_when_no_ticket_key() {
        let ctx = test_ctx();
        let payload = "x".repeat(500);
        let (outcome, path) = cap_oversized_result(Ok(payload.clone()), &ctx, "call-1", 100);
        assert_eq!(outcome.unwrap(), payload);
        assert!(path.is_none(), "no ticket key means no offload");
    }

    #[test]
    fn cap_aggregate_offloads_largest_first() {
        let (ctx, _queue, key, dir) = ticket_ctx();
        // Sizes chosen so the stub's own bytes (~200) don't dominate.
        let small = "a".repeat(40_000);
        let big = "b".repeat(80_000);
        let tiny = "c".repeat(30_000);
        let mut results = vec![
            (tool_result_block("c1", &small), Ok(small.clone()), None),
            (tool_result_block("c2", &big), Ok(big.clone()), None),
            (tool_result_block("c3", &tiny), Ok(tiny.clone()), None),
        ];
        cap_aggregate_outputs(&mut results, &ctx, 100_000);
        // c2 (the largest) was offloaded; the other two stayed inline.
        match &results[1].0 {
            ContentBlock::ToolResult { content, .. } => {
                assert!(content.starts_with("<persisted-output>"));
                assert!(content.contains("Full output saved to:"));
            }
            _ => panic!("expected ToolResult"),
        }
        let big_path = results[1].2.clone().expect("c2 path recorded");
        assert_eq!(big_path, relative_outputs_path(&key, "c2"));
        let body = std::fs::read_to_string(absolute_outputs_path(dir.path(), &key, "c2")).unwrap();
        assert_eq!(body, big);

        assert!(matches!(
            &results[0].0,
            ContentBlock::ToolResult { content, .. } if content.len() == 40_000
        ));
        assert!(matches!(
            &results[2].0,
            ContentBlock::ToolResult { content, .. } if content.len() == 30_000
        ));
        assert!(results[0].2.is_none());
        assert!(results[2].2.is_none());
    }

    #[test]
    fn cap_aggregate_stops_when_only_small_results_remain() {
        let (ctx, _queue, _key, _dir) = ticket_ctx();
        // Many small results whose total far exceeds the cap, but
        // each is already a stub-marked block. Aggregate should bail
        // rather than spin: stubs are skipped, so no candidates.
        let mut results: Vec<(
            ContentBlock,
            std::result::Result<String, ToolError>,
            Option<PathBuf>,
        )> = (0..5)
            .map(|i| {
                let id = format!("c{i}");
                let stub = format!("<persisted-output>already stubbed {i}</persisted-output>");
                (tool_result_block(&id, &stub), Ok(stub), None)
            })
            .collect();
        let before: Vec<String> = results
            .iter()
            .map(|(b, _, _)| match b {
                ContentBlock::ToolResult { content, .. } => content.clone(),
                _ => String::new(),
            })
            .collect();
        cap_aggregate_outputs(&mut results, &ctx, 10);
        let after: Vec<String> = results
            .iter()
            .map(|(b, _, _)| match b {
                ContentBlock::ToolResult { content, .. } => content.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(
            before, after,
            "aggregate must be a no-op when only stubs remain"
        );
    }

    #[test]
    fn format_oversized_tool_result_renders_template() {
        let path = PathBuf::from("/tmp/agentwerk/tickets/TICKET-1/outputs/call-1.txt");
        let stub = format_oversized_tool_result(1_048_576, &path, "preview-body");
        assert!(stub.starts_with("<persisted-output>"));
        assert!(stub.contains("Output too large (1.0 MB)."));
        assert!(stub
            .contains("Full output saved to: /tmp/agentwerk/tickets/TICKET-1/outputs/call-1.txt"));
        assert!(stub.contains("Preview (first 12 B):"));
        assert!(stub.contains("preview-body"));
        assert!(stub.ends_with("</persisted-output>"));
    }

    #[test]
    fn truncate_preview_snaps_at_last_newline_in_window() {
        let mut content = String::new();
        // Build a payload where the last newline within PREVIEW_CHARS is
        // at byte 1_900.
        content.push_str(&"a".repeat(1_900));
        content.push('\n');
        content.push_str(&"b".repeat(500));
        let preview = truncate_preview(&content);
        assert_eq!(preview.len(), 1_901);
        assert!(preview.ends_with('\n'));
    }

    #[test]
    fn truncate_preview_falls_back_to_utf8_boundary_when_no_newline() {
        let content = "x".repeat(3_000);
        let preview = truncate_preview(&content);
        assert_eq!(preview.len(), PREVIEW_CHARS);
        assert!(content.is_char_boundary(preview.len()));
    }

    #[test]
    fn truncate_preview_does_not_split_a_multibyte_char_at_the_window() {
        // A 3-byte char straddling PREVIEW_CHARS must not be sliced through: the
        // window floors to the char boundary below 2000 instead of panicking.
        let mut content = "x".repeat(PREVIEW_CHARS - 1);
        content.push('世'); // occupies bytes 1999..2002, crossing the 2000 window
        content.push_str(&"y".repeat(500));
        let preview = truncate_preview(&content);
        assert!(content.is_char_boundary(preview.len()));
        assert!(preview.len() <= PREVIEW_CHARS);
    }

    #[test]
    fn replace_empty_output_substitutes_placeholder() {
        let outcome: std::result::Result<String, ToolError> = Ok(String::new());
        let outcome = replace_empty_output(outcome, "bash");
        assert_eq!(outcome.unwrap(), "(bash completed with no output)");
    }

    #[test]
    fn replace_empty_output_passes_non_empty_through() {
        let outcome: std::result::Result<String, ToolError> = Ok("hello".into());
        let outcome = replace_empty_output(outcome, "bash");
        assert_eq!(outcome.unwrap(), "hello");
    }

    #[test]
    fn replace_empty_output_passes_errors_through() {
        let outcome: std::result::Result<String, ToolError> = Err(ToolError::ExecutionFailed {
            tool_name: "bash".into(),
            message: "boom".into(),
        });
        let outcome = replace_empty_output(outcome, "bash");
        assert!(matches!(outcome, Err(ToolError::ExecutionFailed { .. })));
    }
}
