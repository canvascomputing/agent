//! Shell access for agents, narrowed to the commands an operator allows and widened to every command only on request.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;
use super::util::{glob_match, run_shell_command};
use crate::providers::ProviderResult as Result;

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("bash.tool.md")))
}

/// Markdown description shared by every restricted `BashTool`. The
/// per-instance patterns are appended at construction time so this base stays
/// runtime-agnostic.
fn description_base() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

/// Execute shell commands. A tool named `git` runs the bare `git` and nothing
/// else until [`BashTool::allow`] widens it; [`BashTool::deny`] overrules any
/// allowed pattern. [`BashTool::unrestricted`] permits every command. Not
/// read-only.
///
/// # Examples
///
/// Restricted to a command family, minus the one call that publishes:
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::BashTool;
///
/// Agent::new().tool(BashTool::new("git").allow("git *").deny("git push*"));
/// ```
///
/// Unrestricted:
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::BashTool;
///
/// Agent::new().tool(BashTool::unrestricted());
/// ```
#[derive(Clone)]
pub struct BashTool {
    tool_name: String,
    allow: Vec<String>,
    deny: Vec<String>,
    description: String,
    custom_description: bool,
    read_only: bool,
}

impl BashTool {
    /// Default per-command timeout when the model omits `timeout_ms`.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(120_000);

    /// Maximum per-command timeout the model is allowed to request.
    pub const MAX_TIMEOUT: Duration = Duration::from_millis(600_000);

    /// Create a new `BashTool` the model calls by `name`. With no
    /// [`BashTool::allow`] pattern it permits the bare `name` and nothing else.
    /// The static description rendered from `bash.tool.md` is loaded once and
    /// the allowed and denied patterns are appended per instance. `read_only`
    /// defaults to the value declared in `bash.tool.md` and may be overridden
    /// via [`BashTool::read_only`].
    pub fn new(name: &str) -> Self {
        let mut tool = Self {
            tool_name: name.to_string(),
            allow: Vec::new(),
            deny: Vec::new(),
            description: String::new(),
            custom_description: false,
            read_only: tool_file().read_only,
        };
        tool.render_description();
        tool
    }

    /// Permit commands matching `pattern`. The first call replaces the
    /// bare-name default, so what is listed is what runs.
    pub fn allow(mut self, pattern: &str) -> Self {
        self.allow.push(pattern.trim().to_string());
        self.render_description();
        self
    }

    /// Refuse commands matching `pattern`, even when an allowed pattern
    /// matches them too.
    pub fn deny(mut self, pattern: &str) -> Self {
        self.deny.push(pattern.trim().to_string());
        self.render_description();
        self
    }

    /// Override the auto-generated description.
    pub fn description(mut self, description: &str) -> Self {
        self.description = description.to_string();
        self.custom_description = true;
        self
    }

    /// Set whether this tool is considered read-only.
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Append the current rules to the shared base description. A caller's own
    /// description wins: they asked for that text, not for this one.
    fn render_description(&mut self) {
        if self.custom_description {
            return;
        }

        let mut description = format!("{}\n\n{}", description_base(), self.allowed_line());
        if !self.deny.is_empty() {
            description.push_str(&format!("\nDenied: {}.", quoted(&self.deny)));
        }
        self.description = description;
    }

    fn allowed_line(&self) -> String {
        if self.allow.is_empty() {
            return format!("Allowed: only the bare command `{}`.", self.tool_name);
        }
        format!("Allowed: {}.", quoted(&self.allow))
    }

    /// The rule refusing `command`, or `None` when it may run.
    fn refusal(&self, command: &str) -> Option<String> {
        let denied = self
            .deny
            .iter()
            .find(|pattern| glob_match(pattern, command));

        if let Some(pattern) = denied {
            return Some(format!(
                "Command '{command}' matches the denied pattern '{pattern}'"
            ));
        }

        let permitted = if self.allow.is_empty() {
            command == self.tool_name
        } else {
            self.allow
                .iter()
                .any(|pattern| glob_match(pattern, command))
        };

        if permitted {
            return None;
        }

        Some(format!(
            "Command '{command}' is not allowed by tool '{name}'. {allowed}",
            name = self.tool_name,
            allowed = self.allowed_line(),
        ))
    }
}

fn quoted(patterns: &[String]) -> String {
    patterns
        .iter()
        .map(|pattern| format!("`{pattern}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl ToolLike for BashTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        tool_file().input_schema.clone()
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn call<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
        Box::pin(async move {
            let command = match input.get("command").and_then(|v| v.as_str()) {
                Some(cmd) => cmd,
                None => return Ok(ToolResult::error("Missing required field: command")),
            };

            if let Some(refusal) = self.refusal(command) {
                return Ok(ToolResult::error(refusal));
            }

            let timeout = input
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .map(Duration::from_millis)
                .unwrap_or(Self::DEFAULT_TIMEOUT);

            Ok(run_shell_command(command, timeout, ctx).await)
        })
    }
}

impl BashTool {
    /// Create an unrestricted bash tool with the standard description.
    pub fn unrestricted() -> Self {
        Self::new(&tool_file().name)
            .allow("*")
            .description(&format!(
                "\
Executes a bash command in the working directory and returns its output.

IMPORTANT: Avoid using this tool when a dedicated tool exists:
- File search: Use glob (NOT find or ls)
- Content search: Use grep (NOT shell grep/rg via bash)
- Read files: Use read_file (NOT cat/head/tail)
- Edit files: Use edit_file (NOT sed/awk)
- Write files: Use write_file (NOT echo/heredoc)

# Instructions
- Always quote file paths that contain spaces with double quotes.
- Try to maintain your current working directory by using absolute paths.
- You may specify an optional timeout in milliseconds (default: {default}, max: {max}).

When issuing multiple commands:
- If commands are independent, make multiple tool calls in parallel.
- If commands depend on each other, chain with && in a single call.
- Do NOT use newlines to separate commands.

# Anti-patterns
- Do not sleep between commands that can run immediately.
- Do not retry failing commands in a sleep loop: diagnose the root cause.
- Do not use interactive flags (-i) as they require input which is not supported.",
                default = Self::DEFAULT_TIMEOUT.as_millis(),
                max = Self::MAX_TIMEOUT.as_millis(),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool_context() -> ToolContext {
        ToolContext::new(std::env::current_dir().unwrap())
    }

    #[test]
    fn an_unrestricted_tool_is_named_bash_and_is_not_read_only() {
        let tool = BashTool::unrestricted();
        assert_eq!(tool.name(), "bash");
        assert!(!tool.is_read_only());
    }

    #[test]
    fn bash_tool_takes_its_name_from_its_only_argument() {
        let tool = BashTool::new("echo");
        assert_eq!(tool.name(), "echo");
    }

    #[test]
    fn bash_tool_can_be_marked_read_only() {
        let tool = BashTool::new("echo").read_only(true);
        assert!(tool.is_read_only());
    }

    #[test]
    fn bash_tool_description_lists_the_allowed_and_denied_patterns() {
        let tool = BashTool::new("git").allow("git status").deny("git push*");
        let description = ToolLike::description(&tool);
        assert!(description.contains("Allowed: `git status`."));
        assert!(description.contains("Denied: `git push*`."));
    }

    #[test]
    fn bash_tool_description_names_the_bare_command_without_an_allowed_pattern() {
        let tool = BashTool::new("git");
        assert!(ToolLike::description(&tool).contains("Allowed: only the bare command `git`."));
    }

    #[test]
    fn bash_tool_custom_description_survives_a_later_allow() {
        let tool = BashTool::new("git")
            .description("Run git commands.")
            .allow("git *");
        assert_eq!(ToolLike::description(&tool), "Run git commands.");
    }

    #[tokio::test]
    async fn a_command_returns_its_output() {
        let tool = BashTool::unrestricted();
        let ctx = test_tool_context();
        let input = serde_json::json!({ "command": "echo hello" });
        let result = tool.call(input, &ctx).await.unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(content.contains("hello"));
        assert!(matches!(result, ToolResult::Success(_)));
    }

    #[tokio::test]
    async fn a_command_over_its_timeout_is_killed() {
        let tool = BashTool::unrestricted();
        let ctx = test_tool_context();
        let input = serde_json::json!({ "command": "sleep 10", "timeout_ms": 100 });
        let result = tool.call(input, &ctx).await.unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("timed out"));
    }

    #[tokio::test]
    async fn a_failing_command_returns_an_error() {
        let tool = BashTool::unrestricted();
        let ctx = test_tool_context();
        let input = serde_json::json!({ "command": "nonexistent_command_xyz" });
        let result = tool.call(input, &ctx).await.unwrap();
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn a_call_without_a_command_is_rejected() {
        let tool = BashTool::unrestricted();
        let ctx = test_tool_context();
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("Missing required field: command"));
    }

    #[tokio::test]
    async fn a_tool_without_an_allowed_pattern_runs_the_bare_command() {
        let tool = BashTool::new("echo");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Success(_)));
    }

    #[tokio::test]
    async fn a_tool_without_an_allowed_pattern_rejects_a_command_with_arguments() {
        let tool = BashTool::new("echo");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo hello" }), &ctx)
            .await
            .unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("is not allowed by tool 'echo'"));
    }

    #[tokio::test]
    async fn an_allowed_pattern_widens_the_bare_command() {
        let tool = BashTool::new("echo").allow("echo *");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo hello" }), &ctx)
            .await
            .unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(matches!(result, ToolResult::Success(_)));
        assert!(content.contains("hello"));
    }

    #[tokio::test]
    async fn a_command_matching_no_allowed_pattern_is_rejected() {
        let tool = BashTool::new("echo").allow("echo *");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "rm -rf /" }), &ctx)
            .await
            .unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("Allowed: `echo *`."));
    }

    #[tokio::test]
    async fn every_allowed_pattern_is_accepted() {
        let tool = BashTool::new("echo").allow("echo one*").allow("echo two*");
        let ctx = test_tool_context();
        for command in ["echo one", "echo two"] {
            let result = tool
                .call(serde_json::json!({ "command": command }), &ctx)
                .await
                .unwrap();
            assert!(matches!(result, ToolResult::Success(_)));
        }
    }

    #[tokio::test]
    async fn a_denied_pattern_overrules_an_allowed_one() {
        let tool = BashTool::new("echo").allow("echo *").deny("echo secret*");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo secret" }), &ctx)
            .await
            .unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("denied pattern 'echo secret*'"));
    }

    #[tokio::test]
    async fn a_denied_pattern_overrules_the_bare_command() {
        let tool = BashTool::new("echo").deny("echo");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn a_denied_command_never_reaches_the_shell() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let tool = BashTool::new("touch")
            .allow("touch *")
            .deny("touch marker*");
        let ctx = ToolContext::new(dir.path().to_path_buf());

        tool.call(serde_json::json!({ "command": "touch allowed.txt" }), &ctx)
            .await
            .unwrap();
        let result = tool
            .call(serde_json::json!({ "command": "touch marker.txt" }), &ctx)
            .await
            .unwrap();

        // Without the allowed file, the absent marker would prove nothing.
        assert!(dir.path().join("allowed.txt").exists());
        assert!(!dir.path().join("marker.txt").exists());
        assert!(matches!(result, ToolResult::Error(_)));
    }
}
