//! The actions agents can take, and the registry an agent's tools live in.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::knowledge::Knowledge;
use crate::agents::tickets::{Run, TicketQueue};
use crate::event::{EventKind, ToolFailureKind};
use crate::schemas::Schema;

/// How many calls one turn runs at the same time. The rest wait their turn.
const MAX_CONCURRENT_CALLS: usize = 10;

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

/// What a tool reports back: a result or an error the model should work
/// around. Both reach the model the same way, and every error counts against
/// `max_schema_retries`.
///
/// The extra fields are agentwerk's bookkeeping, not a tool author's: the
/// constructors below fill them. An oversized result records in `offloaded`
/// where its original went, a repaired call carries its repair notes in
/// `repaired`, and an error's `kind` tags the failure for events and the
/// retry budget. Arguments that miss a schema arrive as an error whose
/// content already carries the schema to match, composed where the rejection
/// happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResult {
    /// The tool ran and produced this text.
    Success {
        content: String,
        /// Where the original went when the content was capped to a stub.
        offloaded: Option<PathBuf>,
        /// One note per repair validation made to accept this call, such as
        /// `/count retyped`. The loop reports each as
        /// [`EventKind::ResponseRepaired`] under the call's tool name.
        repaired: Vec<String>,
    },
    /// The tool failed, and the content tells the model how to recover.
    Error {
        content: String,
        kind: ToolFailureKind,
    },
}

impl ToolResult {
    /// Report a result.
    pub fn success(content: impl Into<String>) -> Self {
        Self::Success {
            content: content.into(),
            offloaded: None,
            repaired: Vec::new(),
        }
    }

    /// Report a failure the model should work around.
    pub fn error(content: impl Into<String>) -> Self {
        Self::Error {
            content: content.into(),
            kind: ToolFailureKind::ExecutionFailed,
        }
    }

    /// Get the text, whichever outcome this is.
    pub fn content(&self) -> &str {
        match self {
            Self::Success { content, .. } | Self::Error { content, .. } => content,
        }
    }

    /// Take the text, whichever outcome this is.
    pub fn into_content(self) -> String {
        match self {
            Self::Success { content, .. } | Self::Error { content, .. } => content,
        }
    }
}

/// The tools one agent may call.
#[derive(Clone, Default)]
pub(crate) struct ToolRegistry {
    tools: Vec<Arc<Tool>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.tools.iter().map(|tool| tool.name()).collect();
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
    pub(crate) fn register(&mut self, tool: impl Into<Tool>) {
        let tool = Arc::new(tool.into());
        self.tools.retain(|t| t.name() != tool.name());
        self.tools.push(tool);
    }

    /// Get the tool a call reaches, owned, so a concurrent batch can move it
    /// into its task, or the message naming what could have been called.
    fn resolve(&self, name: &str) -> std::result::Result<Arc<Tool>, String> {
        if let Some(tool) = self.get(name) {
            return Ok(tool);
        }
        let names = self.names();
        if names.is_empty() {
            return Err(format!("Unknown tool: {name}"));
        }
        Err(format!(
            "Unknown tool: {name}. Available tools: {}",
            names.join(", ")
        ))
    }

    /// Get the tool a call names.
    ///
    /// An exact match wins. Otherwise a spelling that reduces to the same key as
    /// exactly one registered tool resolves to it, so a model that adds a
    /// `_tool` suffix still reaches the right tool.
    pub(crate) fn get(&self, name: &str) -> Option<Arc<Tool>> {
        let name = name.trim();
        if let Some(found) = self.tools.iter().find(|tool| tool.name() == name) {
            return Some(Arc::clone(found));
        }
        let key = lookup_key(name);
        let mut folded = self
            .tools
            .iter()
            .filter(|tool| lookup_key(tool.name()) == key);
        let found = folded.next()?;
        // A key two tools share is ambiguous: refuse rather than guess.
        folded.next().is_none().then(|| Arc::clone(found))
    }

    /// True when a tool of exactly this name is registered. Exact where
    /// [`Self::get`] folds, so a near-miss never passes for the real tool.
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name() == name)
    }

    /// Get the registered names, sorted, for the error that tells the model what
    /// it could have called.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        names.sort();
        names
    }

    /// Get the tools sent to the model.
    pub(crate) fn tools(&self) -> Vec<Tool> {
        self.tools.iter().map(|tool| Tool::clone(tool)).collect()
    }

    /// Run the calls, concurrent ones together and the rest one at a time,
    /// answering each in the order it was asked, with every answer capped to
    /// fit one turn's reply.
    pub(crate) async fn execute(&self, calls: &[ToolCall], ctx: &ToolContext) -> Vec<ToolResult> {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CALLS));
        let mut answers: Vec<Option<ToolResult>> = calls.iter().map(|_| None).collect();

        for batch in partition_tool_calls(calls, self) {
            match batch {
                ToolBatch::Concurrent(batch_calls) => {
                    let answered = self.run_concurrently(batch_calls, ctx, &semaphore).await;
                    for (index, result) in answered {
                        answers[index] = Some(result);
                    }
                }
                ToolBatch::Serial(index, call) => {
                    answers[index] = Some(invoke(self.resolve(&call.name), &call, ctx).await);
                }
            }
        }

        let mut results = answer_every_call(calls, answers);
        cap_results(calls, &mut results, ctx);
        results
    }

    /// Run one batch's calls at the same time, giving back each call's index
    /// with its answer. A call whose task panicked is left out.
    async fn run_concurrently(
        &self,
        batch: Vec<(usize, ToolCall)>,
        ctx: &ToolContext,
        semaphore: &Arc<tokio::sync::Semaphore>,
    ) -> Vec<(usize, ToolResult)> {
        let mut set = tokio::task::JoinSet::new();
        for (index, call) in batch {
            let semaphore = Arc::clone(semaphore);
            let ctx = ctx.clone();
            // Resolved before the spawn: the task outlives this borrow of the
            // registry.
            let resolved = self.resolve(&call.name);
            set.spawn(async move {
                let _permit = semaphore.acquire().await.unwrap();
                (index, invoke(resolved, &call, &ctx).await)
            });
        }

        let mut answers = Vec::new();
        while let Some(joined) = set.join_next().await {
            if let Ok(answer) = joined {
                answers.push(answer);
            }
        }
        answers
    }
}

enum ToolBatch {
    Concurrent(Vec<(usize, ToolCall)>),
    Serial(usize, ToolCall),
}

/// Split a turn's calls into runs of concurrent ones and the serial calls
/// between them, each call carrying the index it was asked at.
fn partition_tool_calls(calls: &[ToolCall], registry: &ToolRegistry) -> Vec<ToolBatch> {
    let mut batches: Vec<ToolBatch> = Vec::new();
    let mut concurrent_batch: Vec<(usize, ToolCall)> = Vec::new();

    for (index, call) in calls.iter().enumerate() {
        let concurrent = registry
            .get(&call.name)
            .is_some_and(|tool| tool.is_concurrent());
        if concurrent {
            concurrent_batch.push((index, call.clone()));
            continue;
        }

        if !concurrent_batch.is_empty() {
            batches.push(ToolBatch::Concurrent(std::mem::take(&mut concurrent_batch)));
        }
        batches.push(ToolBatch::Serial(index, call.clone()));
    }

    if !concurrent_batch.is_empty() {
        batches.push(ToolBatch::Concurrent(concurrent_batch));
    }

    batches
}

/// Give every call an answer, standing in for one whose task panicked: a reply
/// with a `tool_use` block and no result upsets LLM providers.
fn answer_every_call(calls: &[ToolCall], answers: Vec<Option<ToolResult>>) -> Vec<ToolResult> {
    calls
        .iter()
        .zip(answers)
        .map(|(call, answer)| {
            answer.unwrap_or_else(|| {
                ToolResult::error(format!("`{}` did not finish: it panicked", call.name))
            })
        })
        .collect()
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

type ToolHandler = Arc<
    dyn Fn(Value, &ToolContext) -> Pin<Box<dyn Future<Output = ToolResult> + Send + '_>>
        + Send
        + Sync,
>;

/// Collects what a [`Tool`] needs. `.build()` exists only once a description
/// and a handler are installed, so a tool an agent cannot read or run is
/// unrepresentable.
pub struct ToolBuilder<D, H> {
    name: String,
    description: D,
    schema: Schema,
    concurrent: bool,
    paths: Vec<String>,
    handler: H,
}

/// A tool an agent may call: a name, a description, a JSON Schema for the
/// arguments, and the handler that runs.
///
/// The handler names its own argument type, so a typed tool deserializes
/// nowhere in its body; `Value` takes the JSON as it arrived. State the
/// handler needs is captured in its closure. Copying a `Tool` is cheap and
/// shares the handler.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::{Tool, ToolResult};
/// use serde_json::{json, Value};
///
/// let greet = Tool::new("greet")
///     .description("Say hello to a name.")
///     .schema(json!({
///         "type": "object",
///         "properties": { "name": { "type": "string" } },
///         "required": ["name"]
///     }))
///     .concurrent(true)
///     .handler(|input: Value, _ctx| async move {
///         let name = input["name"].as_str().unwrap_or("world");
///         ToolResult::success(format!("Hello, {name}!"))
///     })
///     .build();
///
/// Agent::new().tool(greet);
/// ```
///
/// A tool the model could not read, or that runs nothing, has no `build()`:
///
/// ```compile_fail
/// use agentwerk::tools::Tool;
///
/// let unusable = Tool::new("greet").build();
/// ```
#[derive(Clone)]
pub struct Tool {
    name: String,
    description: String,
    schema: Schema,
    concurrent: bool,
    paths: Vec<String>,
    handler: ToolHandler,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.name)
            .field("concurrent", &self.concurrent)
            .finish()
    }
}

impl Tool {
    /// Start building the tool the model calls by `name`. Say what it does with
    /// `.description(...)`, what it accepts with `.schema(...)`, what it runs
    /// with `.handler(...)`, and finish with `.build()`.
    pub fn new(name: impl Into<String>) -> ToolBuilder<(), ()> {
        ToolBuilder {
            name: name.into(),
            description: (),
            schema: Schema::new(serde_json::json!({"type": "object", "properties": {}}))
                .expect("a literal object schema compiles"),
            concurrent: false,
            paths: Vec::new(),
            handler: (),
        }
    }

    /// Run the tool on a call the registry has already checked against
    /// [`input_schema`](Self::input_schema).
    pub async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        (self.handler)(input, ctx).await
    }

    /// The name the model calls the tool by.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the tool does, in the words the model reads.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The arguments this tool accepts, compiled.
    pub fn input_schema(&self) -> &Schema {
        &self.schema
    }

    /// Whether the agent may run this tool alongside the turn's other
    /// concurrent calls.
    pub fn is_concurrent(&self) -> bool {
        self.concurrent
    }

    /// The file paths this call opens, read from the fields the builder's
    /// `paths` named, so they reach `Stats`.
    pub fn opened_paths(&self, input: &Value) -> Vec<String> {
        self.paths
            .iter()
            .filter_map(|field| input.get(field).and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect()
    }
}

impl<D, H> ToolBuilder<D, H> {
    /// Define what the tool accepts, as a JSON Schema document or the text of
    /// one. Panics on a document `Schema` refuses, naming this tool: an
    /// uncheckable tool is a mistake here, not one the agent should discover at
    /// call time.
    pub fn schema<S>(mut self, schema: S) -> Self
    where
        S: TryInto<Schema>,
        S::Error: std::fmt::Display,
    {
        self.schema = schema.try_into().unwrap_or_else(|error| {
            panic!(
                "tool `{}` declares a schema that does not compile: {error}",
                self.name
            )
        });
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

impl<H> ToolBuilder<(), H> {
    /// Say what the tool does, in the words the model reads. A built-in reads
    /// its own from the `.tool.md` beside it, which is why the text is stored
    /// trimmed: the file's closing newline would otherwise reach the model.
    pub fn description(self, description: impl Into<String>) -> ToolBuilder<String, H> {
        ToolBuilder {
            name: self.name,
            description: description.into().trim().to_string(),
            schema: self.schema,
            concurrent: self.concurrent,
            paths: self.paths,
            handler: self.handler,
        }
    }
}

impl<D> ToolBuilder<D, ()> {
    /// Define what runs when the model calls this tool. A bare `async` block
    /// works, and the handler names the type its arguments are read into.
    pub fn handler<A, F, Fut>(self, handler: F) -> ToolBuilder<D, ToolHandler>
    where
        A: serde::de::DeserializeOwned + Send + 'static,
        F: Fn(A, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        ToolBuilder {
            handler: read_arguments_then(self.name.clone(), handler),
            name: self.name,
            description: self.description,
            schema: self.schema,
            concurrent: self.concurrent,
            paths: self.paths,
        }
    }
}

impl ToolBuilder<String, ToolHandler> {
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

/// Fold a typed handler into the stored form: deserialize the checked
/// arguments, then run the handler on what they read as.
fn read_arguments_then<A, F, Fut>(name: String, handler: F) -> ToolHandler
where
    A: serde::de::DeserializeOwned + Send + 'static,
    F: Fn(A, ToolContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ToolResult> + Send + 'static,
{
    Arc::new(move |input, ctx| match serde_json::from_value::<A>(input) {
        Ok(args) => Box::pin(handler(args, ctx.clone())),
        // The schema accepted this call, so the tool's schema and its
        // argument type disagree: the author's mistake, not the model's.
        // Reporting it as a schema failure would show the model a document
        // its call already satisfied.
        Err(error) => Box::pin(std::future::ready(ToolResult::error(format!(
            "`{name}` could not read its arguments: {error}"
        )))),
    })
}

/// Check the arguments against the schema the tool registered, then run it on
/// what survives. A rejection answers with the complete corrective message,
/// composed here where the schema is known.
async fn invoke(
    resolved: std::result::Result<Arc<Tool>, String>,
    call: &ToolCall,
    ctx: &ToolContext,
) -> ToolResult {
    let tool = match resolved {
        Ok(tool) => tool,
        Err(content) => {
            return ToolResult::Error {
                content,
                kind: ToolFailureKind::ToolNotFound,
            }
        }
    };
    // Retyped rather than refused, so a quoted number runs the call the model
    // asked for and arguments it wrote as JSON text are decoded. What comes
    // back names the value that produced, which is the one the tool would have
    // received.
    let (input, repairs) = match tool.input_schema().validate(call.input.clone()) {
        Ok(validated) => validated,
        Err(violations) => {
            return ToolResult::Error {
                content: crate::prompts::arguments_retry_detail(
                    tool.name(),
                    &violations.to_string(),
                    Some(tool.input_schema().get_raw_schema()),
                ),
                kind: ToolFailureKind::SchemaValidationFailed,
            };
        }
    };
    let mut result = tool.call(input, ctx).await;
    // Argument retypes go in front of notes the tool itself added, keeping
    // the notes in the order the repairs happened.
    if let ToolResult::Success { repaired, .. } = &mut result {
        repaired.splice(0..0, repairs.iter().map(|pointer| retype_message(pointer)));
    }
    result
}

/// Name a retype by the JSON pointer it happened at; the empty pointer is the
/// value as a whole. One verb covers both rewrites: decoding a payload the
/// model wrote as text is the whole-value case of retyping it.
pub(crate) fn retype_message(pointer: &str) -> String {
    match pointer {
        "" => "retyped".to_string(),
        path => format!("{path} retyped"),
    }
}

/// Stand in for every empty and oversized result, so one turn's reply always
/// fits, recording on each capped result where its original went.
///
/// `results` answer `calls`, in the same order. Only a
/// [`ToolResult::Success`] is ever rewritten: a failure's message is what the
/// model must read to recover, and is short by construction.
fn cap_results(calls: &[ToolCall], results: &mut [ToolResult], ctx: &ToolContext) {
    for (call, result) in calls.iter().zip(results.iter_mut()) {
        replace_empty_output(result, &call.name);
        cap_oversized_result(result, ctx, &call.id, PER_TOOL_CAP);
    }
    cap_aggregate_outputs(calls, results, ctx, PER_TURN_CAP);
}

/// Put a placeholder in place of an empty result, since empty content has upset
/// LLM providers.
fn replace_empty_output(result: &mut ToolResult, tool_name: &str) {
    let ToolResult::Success { content, .. } = result else {
        return;
    };
    if content.is_empty() {
        *content = format!("({tool_name} completed with no output)");
    }
}

/// Replace an oversized result with a stub, writing the original under the
/// ticket's outputs directory.
///
/// A failure passes through, being short by construction, and so does the raw
/// content when the write fails.
fn cap_oversized_result(
    result: &mut ToolResult,
    ctx: &ToolContext,
    call_id: &str,
    per_tool_cap: usize,
) {
    let ToolResult::Success {
        content, offloaded, ..
    } = result
    else {
        return;
    };
    if content.len() <= per_tool_cap {
        return;
    }
    if let Some(path) = write_out(content, ctx, call_id) {
        *offloaded = Some(path);
    }
}

/// While one turn's results are too large together, write out the largest
/// success that is not already a stub. It stops once the turn fits, or once
/// nothing left can be written out.
fn cap_aggregate_outputs(
    calls: &[ToolCall],
    results: &mut [ToolResult],
    ctx: &ToolContext,
    per_turn_cap: usize,
) {
    loop {
        let total: usize = results.iter().map(|result| result.content().len()).sum();
        if total <= per_turn_cap {
            return;
        }
        let Some((call, content, offloaded)) = largest_inline_success(calls, results) else {
            return;
        };
        let Some(path) = write_out(content, ctx, &call.id) else {
            // Persistence failed; nothing further this pass can do.
            return;
        };
        *offloaded = Some(path);
    }
}

/// The largest success still inline, which is the next one to write out.
fn largest_inline_success<'a>(
    calls: &'a [ToolCall],
    results: &'a mut [ToolResult],
) -> Option<(&'a ToolCall, &'a mut String, &'a mut Option<PathBuf>)> {
    calls
        .iter()
        .zip(results.iter_mut())
        .filter_map(|(call, result)| match result {
            ToolResult::Success {
                content, offloaded, ..
            } if !content.starts_with(OVERSIZED_STUB_TAG_OPEN) => Some((call, content, offloaded)),
            _ => None,
        })
        .max_by_key(|(_, content, _)| content.len())
}

/// Write `content` out under the ticket's outputs directory and leave the stub
/// the model reads in its place, giving back where the original went.
///
/// `None` when the write fails, and then `content` is left as it was.
fn write_out(content: &mut String, ctx: &ToolContext, call_id: &str) -> Option<PathBuf> {
    let output = persist_output(ctx, call_id, content)?;
    let preview = truncate_preview(content);
    let stub = format_oversized_tool_result(content.len(), &output.display, preview);
    *content = stub;
    Some(output.rel)
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
        .map(|newline| newline + 1)
        .unwrap_or(window);
    &content[..cut]
}

fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if (bytes as f64) < MB {
        format!("{:.1} KB", bytes as f64 / KB)
    } else {
        format!("{:.1} MB", bytes as f64 / MB)
    }
}

/// Move an index back to the nearest character boundary, at most three bytes.
fn utf8_boundary_floor(content: &str, mut index: usize) -> usize {
    while index > 0 && !content.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool agentwerk registers, for the checks that hold across all of
    /// them. The knowledge store is temporary; its tool is read, not used.
    fn built_in_tools() -> (Vec<Tool>, crate::test_util::TempDir) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = crate::agents::knowledge::Knowledge::load(dir.path()).unwrap();
        let tools: Vec<Tool> = vec![
            crate::tools::ReadFileTool.into(),
            crate::tools::WriteFileTool.into(),
            crate::tools::EditFileTool.into(),
            crate::tools::GlobTool.into(),
            crate::tools::GrepTool.into(),
            crate::tools::ListDirectoryTool.into(),
            crate::tools::FetchUrlTool::new().into(),
            crate::tools::KnowledgeTool::new(store).into(),
            crate::tools::CommandTool::new("git").allow("git *").into(),
            crate::tools::FinishTool.into(),
            crate::tools::TicketsTool.into(),
        ];
        (tools, dir)
    }

    /// Each built-in states its name and concurrency as a literal in its `From`
    /// conversion and reads its description from the file beside it, so a typo
    /// in either literal, or a definition that stopped being included, is what
    /// this pins.
    #[test]
    fn every_built_in_tool_declares_what_the_model_is_shown() {
        let (tools, _dir) = built_in_tools();
        let declared: Vec<(&str, bool)> = tools
            .iter()
            .map(|tool| (tool.name(), tool.is_concurrent()))
            .collect();
        assert_eq!(
            declared,
            [
                ("read_file", true),
                ("write_file", false),
                ("edit_file", false),
                ("glob", true),
                ("grep", true),
                ("list_directory", true),
                ("fetch_url", true),
                ("knowledge", false),
                ("git", false),
                ("finish", false),
                ("tickets", false),
            ]
        );
        for tool in &tools {
            assert!(
                !tool.description().is_empty(),
                "empty description for {}",
                tool.name(),
            );
            // The registry holds a call to this, so a tool that declares
            // something else loses the check that its arguments are an object.
            assert_eq!(
                tool.input_schema().get_raw_schema()["type"],
                "object",
                "arguments are not an object for {}",
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
            let schema = tool.input_schema();
            let examples = schema.get_raw_schema()["examples"]
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
        registry.register(mock_tool("echo", true, "first"));
        registry.register(mock_tool("echo", true, "second"));

        let definitions = registry.tools();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name, "echo");
    }

    #[test]
    fn paths_reports_the_named_input_fields() {
        let tool = Tool::new("cat")
            .description("Read a file.")
            .paths(["path", "into"])
            .handler(|_: Value, _ctx| async move { ToolResult::success("ok") })
            .build();

        let input = serde_json::json!({"path": "src/lib.rs", "limit": 20});
        assert_eq!(tool.opened_paths(&input), vec!["src/lib.rs".to_string()]);
    }

    /// The mock the registry tests share.
    fn mock_tool(name: &str, concurrent: bool, result: &str) -> Tool {
        let result = result.to_string();
        Tool::new(name)
            .description("mock")
            .schema(serde_json::json!({"type": "object"}))
            .concurrent(concurrent)
            .handler(move |_: Value, _ctx| {
                let result = result.clone();
                async move { ToolResult::success(result) }
            })
            .build()
    }

    fn test_ctx() -> ToolContext {
        ToolContext::new(std::env::current_dir().unwrap())
    }

    #[test]
    fn registry_register_and_get() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("read_file", true, "file contents"));
        assert!(registry.get("read_file").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn contains_answers_on_the_exact_name_only() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("grep", true, "matches"));
        assert!(registry.contains("grep"));
        assert!(!registry.contains("glob"));
        assert!(
            !registry.contains("grep_tool"),
            "a folded spelling is not the registered name",
        );
    }

    #[test]
    fn resolves_a_name_carrying_a_tool_suffix() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("grep", true, "matches"));

        let tool = registry.get("grep_tool").expect("suffix should fold away");
        assert_eq!(tool.name(), "grep");
    }

    #[test]
    fn resolves_a_name_the_model_hyphenated() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("read_file", true, "file contents"));

        let tool = registry
            .get("Read-File")
            .expect("case and hyphen should fold");
        assert_eq!(tool.name(), "read_file");
    }

    #[test]
    fn refuses_a_name_two_tools_share_a_key() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("grep", true, "builtin"));
        registry.register(mock_tool("grep_tool", true, "host tool"));

        assert!(registry.get("Grep").is_none());
        // Each registered name still reaches its own tool: exact match wins.
        assert_eq!(registry.get("grep").unwrap().name(), "grep");
        assert_eq!(registry.get("grep_tool").unwrap().name(), "grep_tool");
    }

    #[test]
    fn a_description_read_from_a_file_keeps_its_prose_and_loses_the_closing_newline() {
        let tool = Tool::new("demo")
            .description("Do the demo thing.\n\n- Returns nothing useful.\n")
            .handler(|_: Value, _| async { ToolResult::success("") })
            .build();
        assert_eq!(
            tool.description(),
            "Do the demo thing.\n\n- Returns nothing useful."
        );
    }

    #[test]
    fn a_schema_written_as_json_text_is_the_one_the_tool_advertises() {
        let tool = Tool::new("demo")
            .description("Do the demo thing.")
            .schema(
                r#"{"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]}"#,
            )
            .handler(|_: Value, _| async { ToolResult::success("") })
            .build();
        let document = tool.input_schema().get_raw_schema();
        assert_eq!(document["properties"]["x"]["type"], "string");
        assert_eq!(document["required"][0], "x");
    }

    #[test]
    fn registry_lists_every_registered_tool() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("read", true, "ok"));
        registry.register(mock_tool("write", false, "ok"));

        let tools = registry.tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name(), "read");
        assert_eq!(tools[1].name(), "write");
    }

    #[tokio::test]
    async fn a_rejected_call_reads_back_the_one_schema_its_tool_holds() {
        let ticket = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"partial_sum": {"type": "integer"}},
            "required": ["partial_sum"],
        }))
        .unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(crate::tools::FinishTool::from_schema(Some(ticket)));

        let shown = registry
            .get("finish")
            .expect("finish is registered")
            .input_schema()
            .get_raw_schema()
            .clone();
        let calls = vec![ToolCall {
            id: "c1".to_string(),
            name: "finish".to_string(),
            input: serde_json::json!({}),
        }];
        let content = registry.execute(&calls, &test_ctx()).await[0]
            .content()
            .to_string();

        assert!(
            shown["properties"]["result"]["properties"]["partial_sum"].is_object(),
            "{shown}"
        );
        assert!(
            content.contains(&serde_json::to_string_pretty(&shown).unwrap()),
            "{content}"
        );
    }

    #[test]
    fn registry_clone() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("t", true, "ok"));
        let cloned = registry.clone();
        assert_eq!(cloned.tools().len(), 1);
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
        assert!(matches!(results[0], ToolResult::Error { .. }));
        assert!(results[0].content().contains("Unknown tool"));
    }

    #[tokio::test]
    async fn non_object_input_is_reported_as_such_and_never_reaches_the_tool() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("grep", true, "matches"));
        let ctx = test_ctx();
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "grep".into(),
            input: Value::String(r#"{"pattern": "exec""#.into()),
        }];

        let results = registry.execute(&calls, &ctx).await;
        assert!(matches!(
            results[0],
            ToolResult::Error {
                kind: ToolFailureKind::SchemaValidationFailed,
                ..
            }
        ));
        let content = results[0].content();
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
    fn typed_tool() -> Tool {
        Tool::new("typed")
            .description("typed")
            .schema(serde_json::json!({
                "type": "object",
                "properties": { "count": { "type": "integer" } },
                "required": ["count"],
            }))
            .handler(
                |input: Value, _ctx| async move { ToolResult::success(input["count"].to_string()) },
            )
            .build()
    }

    async fn call_typed(input: Value) -> (String, bool) {
        call_typed_in(input, &test_ctx()).await
    }

    async fn call_typed_in(input: Value, ctx: &ToolContext) -> (String, bool) {
        let mut registry = ToolRegistry::default();
        registry.register(typed_tool());
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "typed".into(),
            input,
        }];
        let mut results = registry.execute(&calls, ctx).await;
        let result = results.remove(0);
        let succeeded = matches!(result, ToolResult::Success { .. });
        (result.into_content(), succeeded)
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

    /// One retyped call's result, for the tests reading its repair notes.
    async fn typed_result(input: Value) -> ToolResult {
        let mut registry = ToolRegistry::default();
        registry.register(typed_tool());
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "typed".into(),
            input,
        }];
        registry.execute(&calls, &test_ctx()).await.remove(0)
    }

    #[tokio::test]
    async fn a_retyped_argument_is_noted_on_the_result() {
        let result = typed_result(serde_json::json!({"count": "3"})).await;
        assert!(matches!(
            &result,
            ToolResult::Success { repaired, .. } if repaired == &vec!["/count retyped".to_string()]
        ));

        // No pointer names a whole payload the model wrote as text.
        let result = typed_result(Value::String(r#"{"count": 3}"#.into())).await;
        assert!(matches!(
            &result,
            ToolResult::Success { repaired, .. } if repaired == &vec!["retyped".to_string()]
        ));
    }

    #[tokio::test]
    async fn an_argument_the_schema_does_not_declare_still_reaches_the_tool() {
        // Tool schemas close over nothing: an extra key is generosity, not error.
        let (_, succeeded) = call_typed(serde_json::json!({"count": 3, "extra": true})).await;
        assert!(succeeded);
    }

    #[tokio::test]
    async fn unknown_tool_error_names_the_registered_tools() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("grep", true, "matches"));
        registry.register(mock_tool("read_file", true, "file contents"));
        let ctx = test_ctx();
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "ripgrep".into(),
            input: serde_json::json!({}),
        }];

        let results = registry.execute(&calls, &ctx).await;
        assert_eq!(
            results[0].content(),
            "Unknown tool: ripgrep. Available tools: grep, read_file"
        );
    }

    #[tokio::test]
    async fn execute_concurrent_tools_at_the_same_time() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("read1", true, "result1"));
        registry.register(mock_tool("read2", true, "result2"));
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_whose_task_panics_is_still_answered() {
        // Leaving the slot empty would send the model a `tool_use` block with
        // no result, which providers reject.
        let mut registry = ToolRegistry::default();
        registry.register(
            Tool::new("explode")
                .description("panics")
                .concurrent(true)
                .handler(|_: Value, _ctx| async {
                    panic!("boom");
                })
                .build(),
        );
        registry.register(mock_tool("steady", true, "ok"));
        let ctx = test_ctx();
        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "explode".into(),
                input: serde_json::json!({}),
            },
            ToolCall {
                id: "c2".into(),
                name: "steady".into(),
                input: serde_json::json!({}),
            },
        ];

        let results = registry.execute(&calls, &ctx).await;

        assert_eq!(results.len(), 2);
        assert!(
            matches!(&results[0], ToolResult::Error { content, .. } if content.contains("panicked")),
            "{:?}",
            results[0]
        );
        assert_eq!(results[1].content(), "ok");
    }

    #[tokio::test]
    async fn execute_serial_tool() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("write_file", false, "written"));
        let ctx = test_ctx();

        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            input: serde_json::json!({"path": "/tmp/test"}),
        }];

        let results = registry.execute(&calls, &ctx).await;
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], ToolResult::Success { .. }));
        assert_eq!(results[0].content(), "written");
    }

    #[tokio::test]
    async fn a_typed_handler_that_cannot_read_its_arguments_names_the_author() {
        // The default schema accepts `{}`, so the mismatch is between the
        // schema and the handler's argument type: the author's mistake.
        #[derive(serde::Deserialize)]
        struct Args {
            #[allow(dead_code)]
            count: i64,
        }
        let tool = Tool::new("typed")
            .description("typed")
            .handler(|_: Args, _ctx| async move { ToolResult::success("ran") })
            .build();
        let outcome = tool.call(serde_json::json!({}), &test_ctx()).await;
        assert!(
            matches!(&outcome, ToolResult::Error { content, .. }
                if content.contains("`typed` could not read its arguments")),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_cloned_tool_runs_the_same_handler() {
        // The bindings register one tool on several agents; the clones must
        // share the handler, not lose it.
        let tool = Tool::new("echo")
            .description("Echoes input")
            .handler(|input: Value, _ctx| async move {
                ToolResult::success(input["text"].as_str().unwrap_or("").to_string())
            })
            .build();
        let cloned = tool.clone();
        let outcome = cloned
            .call(serde_json::json!({"text": "hi"}), &test_ctx())
            .await;
        assert_eq!(outcome.content(), "hi");
        assert_eq!(cloned.name(), tool.name());
    }

    #[test]
    fn tool_basic() {
        let tool = Tool::new("echo")
            .description("Echoes input")
            .schema(
                serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            )
            .concurrent(true)
            .handler(|input: Value, _ctx| async move {
                let text = input["text"].as_str().unwrap_or("").to_string();
                ToolResult::success(text)
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

    /// Where a capped result says its original went.
    fn offloaded_path(result: &ToolResult) -> Option<&PathBuf> {
        match result {
            ToolResult::Success { offloaded, .. } => offloaded.as_ref(),
            ToolResult::Error { .. } => None,
        }
    }

    /// The call an aggregate test answers with a synthetic result.
    fn sized_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "dump".into(),
            input: serde_json::json!({}),
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
        let mut result = ToolResult::success("z".repeat(500));
        cap_oversized_result(&mut result, &ctx, "call-rel", 100);
        let stored = offloaded_path(&result).expect("offload happened");
        assert_eq!(stored, &relative_outputs_path(&key, "call-rel"));
        assert!(
            stored.is_relative(),
            "comment path must stay portable: {}",
            stored.display()
        );
    }

    #[test]
    fn persisted_output_renders_absolute_path_for_model() {
        let (ctx, _queue, key, dir) = ticket_ctx();
        let mut result = ToolResult::success("y".repeat(500));
        cap_oversized_result(&mut result, &ctx, "call-abs", 100);
        let absolute = absolute_outputs_path(dir.path(), &key, "call-abs");
        assert!(
            result.content().contains(&absolute.display().to_string()),
            "stub must give the model the joinable on-disk path: {}",
            result.content()
        );
    }

    #[test]
    fn cap_oversized_result_passes_through_under_cap() {
        let ctx = test_ctx();
        let mut result = ToolResult::success("hello");
        cap_oversized_result(&mut result, &ctx, "call-1", 100);
        assert_eq!(result.content(), "hello");
        assert!(offloaded_path(&result).is_none());
    }

    #[test]
    fn cap_oversized_result_replaces_oversized_ok_with_stub() {
        let (ctx, _queue, key, dir) = ticket_ctx();
        let mut result = ToolResult::success("a".repeat(500));
        cap_oversized_result(&mut result, &ctx, "call-xyz", 100);
        let stub = result.content();
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
        let path = offloaded_path(&result).expect("offload path");
        assert_eq!(path, &relative_outputs_path(&key, "call-xyz"));
        let body = std::fs::read_to_string(&absolute).unwrap();
        assert_eq!(body, "a".repeat(500));
    }

    #[test]
    fn cap_oversized_result_passes_a_failure_through() {
        let ctx = test_ctx();
        let mut result = ToolResult::error("boom");
        cap_oversized_result(&mut result, &ctx, "call-1", 1);
        assert_eq!(result.content(), "boom");
        assert!(offloaded_path(&result).is_none());
    }

    #[test]
    fn cap_oversized_result_returns_raw_when_no_ticket_key() {
        let ctx = test_ctx();
        let payload = "x".repeat(500);
        let mut result = ToolResult::success(payload.clone());
        cap_oversized_result(&mut result, &ctx, "call-1", 100);
        assert_eq!(result.content(), payload);
        assert!(
            offloaded_path(&result).is_none(),
            "no ticket key means no offload"
        );
    }

    #[test]
    fn cap_aggregate_offloads_largest_first() {
        let (ctx, _queue, key, dir) = ticket_ctx();
        // Sizes chosen so the stub's own bytes (~200) don't dominate.
        let big = "b".repeat(80_000);
        let calls = vec![sized_call("c1"), sized_call("c2"), sized_call("c3")];
        let mut results = vec![
            ToolResult::success("a".repeat(40_000)),
            ToolResult::success(big.clone()),
            ToolResult::success("c".repeat(30_000)),
        ];
        cap_aggregate_outputs(&calls, &mut results, &ctx, 100_000);
        // c2 (the largest) was offloaded; the other two stayed inline.
        assert!(results[1].content().starts_with("<persisted-output>"));
        assert!(results[1].content().contains("Full output saved to:"));
        assert_eq!(
            offloaded_path(&results[1]),
            Some(&relative_outputs_path(&key, "c2"))
        );
        let body = std::fs::read_to_string(absolute_outputs_path(dir.path(), &key, "c2")).unwrap();
        assert_eq!(body, big);

        assert_eq!(results[0].content().len(), 40_000);
        assert_eq!(results[2].content().len(), 30_000);
        assert!(offloaded_path(&results[0]).is_none());
        assert!(offloaded_path(&results[2]).is_none());
    }

    #[test]
    fn cap_aggregate_stops_when_only_small_results_remain() {
        let (ctx, _queue, _key, _dir) = ticket_ctx();
        // Many small results whose total far exceeds the cap, but
        // each is already stub-marked. Aggregate should bail
        // rather than spin: stubs are skipped, so no candidates.
        let calls: Vec<ToolCall> = (0..5).map(|i| sized_call(&format!("c{i}"))).collect();
        let mut results: Vec<ToolResult> = (0..5)
            .map(|i| {
                ToolResult::success(format!(
                    "<persisted-output>already stubbed {i}</persisted-output>"
                ))
            })
            .collect();
        let before: Vec<String> = results.iter().map(|r| r.content().to_string()).collect();
        cap_aggregate_outputs(&calls, &mut results, &ctx, 10);
        let after: Vec<String> = results.iter().map(|r| r.content().to_string()).collect();
        assert_eq!(
            before, after,
            "aggregate must be a no-op when only stubs remain"
        );
        assert!(results.iter().all(|r| offloaded_path(r).is_none()));
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
        let mut result = ToolResult::success("");
        replace_empty_output(&mut result, "bash");
        assert_eq!(result.content(), "(bash completed with no output)");
    }

    #[test]
    fn replace_empty_output_passes_non_empty_through() {
        let mut result = ToolResult::success("hello");
        replace_empty_output(&mut result, "bash");
        assert_eq!(result.content(), "hello");
    }

    #[test]
    fn replace_empty_output_passes_a_failure_through() {
        // The guard reads the variant, not the content, so even an empty
        // failure message is left as the tool reported it.
        let mut result = ToolResult::error("");
        replace_empty_output(&mut result, "bash");
        assert_eq!(result.content(), "");
    }

    #[test]
    fn cap_aggregate_skips_a_failed_result() {
        // A failure's message is what the model must read to recover, so the
        // aggregate cap never writes it out.
        let (ctx, _queue, _key, _dir) = ticket_ctx();
        let calls = vec![sized_call("c1")];
        let mut results = vec![ToolResult::error("e".repeat(50_000))];
        cap_aggregate_outputs(&calls, &mut results, &ctx, 10);
        assert_eq!(results[0].content().len(), 50_000);
        assert!(offloaded_path(&results[0]).is_none());
    }
}
