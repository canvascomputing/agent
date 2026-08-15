//! Command access for agents, narrowed to the commands an operator names and refused for everything else.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use super::super::tool::{ToolContext, ToolLike, ToolResult};
use super::super::tool_file::ToolFile;
use super::super::util::{glob_match, run_command};
use super::parse::{Argument, Command, Refusal};
use crate::providers::ProviderResult as Result;

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("command.tool.md")))
}

/// The shared part of every tool's description, with the per-instance patterns
/// appended at construction time.
fn description_base() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

/// Execute commands. A tool named `git` runs the bare `git` and nothing else
/// until [`CommandTool::allow`] widens it; [`CommandTool::deny`] and
/// [`CommandTool::deny_flag`] overrule any allowed pattern. Not concurrent.
///
/// One call runs one program, without a shell.
///
/// This is a parsing guarantee, not a privilege boundary. One allowed command
/// can still reach arbitrary code: `git -c alias.x='!cmd' x`, `find -exec`,
/// `ssh -o ProxyCommand`, and any interpreter such as `sh -c` all do. Confine an
/// agent with an operating-system sandbox, not with these rules alone.
///
/// # Examples
///
/// Restricted to a command family, minus the one call that publishes:
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::CommandTool;
///
/// Agent::new().tool(CommandTool::new("git").allow("git *").deny("git push*"));
/// ```
#[derive(Clone)]
pub struct CommandTool {
    tool_name: String,
    allow: Vec<String>,
    deny: Vec<String>,
    deny_flags: Vec<DeniedFlag>,
    description: String,
    custom_description: bool,
    concurrent: bool,
}

impl CommandTool {
    /// Default per-command timeout when the model omits `timeout_ms`.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(120_000);

    /// Maximum per-command timeout the model is allowed to request.
    pub const MAX_TIMEOUT: Duration = Duration::from_millis(600_000);

    /// Create a tool the model calls by `name`. With no [`CommandTool::allow`]
    /// pattern it permits the bare `name` and nothing else.
    pub fn new(name: &str) -> Self {
        let mut tool = Self {
            tool_name: name.to_string(),
            allow: Vec::new(),
            deny: Vec::new(),
            deny_flags: Vec::new(),
            description: String::new(),
            custom_description: false,
            concurrent: tool_file().concurrent,
        };
        tool.render_description();
        tool
    }

    /// Permit commands matching `pattern`. The first call replaces the
    /// bare-name default, so what is listed is what runs.
    ///
    /// The pattern is matched against the program and its arguments joined by
    /// single spaces. Quoting is gone by then, so a quoted argument holding
    /// spaces satisfies the same pattern as separate words while reaching the
    /// program as one argument.
    pub fn allow(mut self, pattern: &str) -> Self {
        self.allow.push(pattern.trim().to_string());
        self.render_description();
        self
    }

    /// Refuse commands matching `pattern`, even when an allowed pattern
    /// matches them too.
    ///
    /// The pattern is matched against the program and its arguments joined by
    /// single spaces, so `git  push` and `git "push"` are caught by the same
    /// `git push*` that catches `git push`.
    pub fn deny(mut self, pattern: &str) -> Self {
        self.deny.push(pattern.trim().to_string());
        self.render_description();
        self
    }

    /// Refuse commands carrying `flag`, wherever it sits in the arguments.
    ///
    /// A single letter reaches every short form holding it, which over-matches
    /// on purpose: `-e` refuses `find -name` too. A deny that is too wide
    /// refuses; one that is too narrow lets the command run.
    ///
    /// Two ways a flag still gets through: arguments after a bare `--` are
    /// operands and go unchecked, though a program with its own parser may
    /// read them as flags anyway; and a program accepting abbreviated long
    /// options reads `--forc` as `--force`, while the rule matches only the
    /// spelling it names.
    ///
    /// # Panics
    ///
    /// When `flag` does not read as one, such as `force` or `-5`: the rule
    /// would otherwise sit inert and deny nothing.
    pub fn deny_flag(mut self, flag: &str) -> Self {
        let flag = flag.trim().to_string();
        assert!(
            matches!(
                Argument::parse(&flag),
                Argument::Long(_) | Argument::Short(_)
            ),
            "deny_flag({flag:?}) is not a flag: name one like \"--force\" or \"-f\""
        );
        self.deny_flags.push(DeniedFlag::new(flag));
        self.render_description();
        self
    }

    /// Override the auto-generated description.
    pub fn description(mut self, description: &str) -> Self {
        self.description = description.to_string();
        self.custom_description = true;
        self
    }

    /// Run this tool in parallel with the turn's other concurrent calls. Set it
    /// for a tool with no side effects.
    pub fn concurrent(mut self, concurrent: bool) -> Self {
        self.concurrent = concurrent;
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
        if !self.deny_flags.is_empty() {
            let written: Vec<String> = self
                .deny_flags
                .iter()
                .map(|denied| denied.written.clone())
                .collect();
            description.push_str(&format!("\nDenied flags: {}.", quoted(&written)));
        }
        self.description = description;
    }

    fn allowed_line(&self) -> String {
        if self.allow.is_empty() {
            return format!("Allowed: only the bare command `{}`.", self.tool_name);
        }
        format!("Allowed: {}.", quoted(&self.allow))
    }

    /// The one command this call runs, or the rule refusing it.
    ///
    /// Rules match the normalized form rather than the line as written, because
    /// that is what runs: otherwise a second space or a pair of quotes walks a
    /// command past a deny that names it.
    fn check(&self, line: &str) -> std::result::Result<Command, String> {
        let line = line.trim();
        let command = Command::split(line).map_err(|refusal| self.unreadable(line, refusal))?;
        let normalized = command.normalized();

        if is_assignment(&command.program) {
            return Err(format!(
                "Command '{normalized}' sets an environment variable. This tool runs one program \
                 with the environment it was started in, so drop the assignment."
            ));
        }

        if let Some((flag, _)) = command.flags().find(|(_, found)| self.denies_flag(*found)) {
            return Err(format!(
                "Command '{normalized}' carries the denied flag '{flag}'"
            ));
        }

        if let Some(pattern) = self
            .deny
            .iter()
            .find(|pattern| glob_match(pattern, &normalized))
        {
            return Err(format!(
                "Command '{normalized}' matches the denied pattern '{pattern}'"
            ));
        }

        let permitted = if self.allow.is_empty() {
            command.program == self.tool_name && command.arguments.is_empty()
        } else {
            self.allow
                .iter()
                .any(|pattern| glob_match(pattern, &normalized))
        };

        if permitted {
            return Ok(command);
        }

        Err(format!(
            "Command '{normalized}' is not allowed by tool '{name}'. {allowed}",
            name = self.tool_name,
            allowed = self.allowed_line(),
        ))
    }

    /// The message for a line that is not one command, naming what stopped it
    /// so the model can fix the call rather than guess at it.
    fn unreadable(&self, line: &str, refusal: Refusal) -> String {
        match refusal {
            Refusal::OperatorFound(operator) => format!(
                "Command '{line}' holds the shell operator `{operator}`. This tool runs one \
                 program directly, with no shell, so make one call per command."
            ),
            Refusal::Unterminated => {
                format!("Command '{line}' ends inside a quote or an escape.")
            }
            Refusal::ControlCharacterFound => {
                format!("Command '{line}' holds a control character.")
            }
            Refusal::Empty => "Missing required field: command".to_string(),
        }
    }

    /// Whether a deny rule names `found`. A long rule and a short one never
    /// meet, so guarding both spellings of one flag takes two calls.
    fn denies_flag(&self, found: Argument<'_>) -> bool {
        self.deny_flags
            .iter()
            .any(|denied| match (&denied.key, found) {
                (FlagKey::Long(name), Argument::Long(found)) => name == found,
                (FlagKey::Letter(letter), Argument::Short(letters)) => letters.contains(*letter),
                (FlagKey::Cluster(spelling), Argument::Short(letters)) => spelling == letters,
                _ => false,
            })
    }
}

/// A deny rule, classified when the builder takes it rather than on every call.
#[derive(Clone)]
struct DeniedFlag {
    /// As the operator wrote it, for the description and the refusal.
    written: String,
    key: FlagKey,
}

impl DeniedFlag {
    /// Split the rule into the form it is matched in. The caller has already
    /// checked that it reads as a flag.
    fn new(written: String) -> Self {
        let key = match Argument::parse(&written) {
            Argument::Long(name) => FlagKey::Long(name.to_string()),
            Argument::Short(letters) => match letters.chars().count() {
                1 => FlagKey::Letter(letters.chars().next().expect("one letter")),
                _ => FlagKey::Cluster(letters.to_string()),
            },
            Argument::Escape | Argument::Operand => unreachable!("deny_flag rejects a non-flag"),
        };
        Self { written, key }
    }
}

/// The ways a rule can name a flag, one variant per matching behaviour.
#[derive(Clone)]
enum FlagKey {
    /// `--force`, which also reaches `--force=x`.
    Long(String),
    /// `-f`, which reaches every short form holding that letter. It cannot tell
    /// a cluster from a value written against a letter, so it over-matches:
    /// only a declaration of which letters take values could separate them.
    Letter(char),
    /// `-rf`, which reaches only that spelling.
    Cluster(String),
}

/// Whether `token` is a `NAME=value` prefix rather than a program.
fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn quoted(patterns: &[String]) -> String {
    patterns
        .iter()
        .map(|pattern| format!("`{pattern}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl ToolLike for CommandTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        tool_file().input_schema.clone()
    }

    fn is_concurrent(&self) -> bool {
        self.concurrent
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

            let command = match self.check(command) {
                Ok(command) => command,
                Err(refusal) => return Ok(ToolResult::error(refusal)),
            };

            let timeout = input
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .map(Duration::from_millis)
                .unwrap_or(Self::DEFAULT_TIMEOUT)
                // The schema advertises the ceiling, so honour it rather than
                // letting a call name any number it likes.
                .min(Self::MAX_TIMEOUT);

            Ok(run_command(&command, timeout, ctx).await)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool_context() -> ToolContext {
        ToolContext::new(std::env::current_dir().unwrap())
    }

    #[test]
    fn a_tool_is_not_concurrent_by_default() {
        assert!(!CommandTool::new("echo").is_concurrent());
    }

    #[test]
    fn a_tool_takes_its_name_from_its_only_argument() {
        let tool = CommandTool::new("echo");
        assert_eq!(tool.name(), "echo");
    }

    #[test]
    fn a_tool_can_be_marked_concurrent() {
        let tool = CommandTool::new("echo").concurrent(true);
        assert!(tool.is_concurrent());
    }

    #[test]
    fn a_description_lists_the_allowed_and_denied_patterns() {
        let tool = CommandTool::new("git")
            .allow("git status")
            .deny("git push*");
        let description = ToolLike::description(&tool);
        assert!(description.contains("Allowed: `git status`."));
        assert!(description.contains("Denied: `git push*`."));
    }

    #[test]
    fn a_description_names_the_bare_command_without_an_allowed_pattern() {
        let tool = CommandTool::new("git");
        assert!(ToolLike::description(&tool).contains("Allowed: only the bare command `git`."));
    }

    #[test]
    fn a_custom_description_survives_a_later_allow() {
        let tool = CommandTool::new("git")
            .description("Run git commands.")
            .allow("git *");
        assert_eq!(ToolLike::description(&tool), "Run git commands.");
    }

    #[tokio::test]
    async fn a_command_returns_its_output() {
        let tool = CommandTool::new("echo").allow("echo *");
        let ctx = test_tool_context();
        let input = serde_json::json!({ "command": "echo hello" });
        let result = tool.call(input, &ctx).await.unwrap();
        let content = result.content();
        assert!(content.contains("hello"));
        assert!(matches!(result, ToolResult::Success(_)));
    }

    #[tokio::test]
    async fn a_command_over_its_timeout_is_killed() {
        let tool = CommandTool::new("sleep").allow("sleep *");
        let ctx = test_tool_context();
        let input = serde_json::json!({ "command": "sleep 10", "timeout_ms": 100 });
        let result = tool.call(input, &ctx).await.unwrap();
        let content = result.content();
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("timed out"));
    }

    #[tokio::test]
    async fn a_call_without_a_command_is_rejected() {
        let tool = CommandTool::new("echo");
        let ctx = test_tool_context();
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        let content = result.content();
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("Missing required field: command"));
    }

    #[tokio::test]
    async fn a_tool_without_an_allowed_pattern_runs_the_bare_command() {
        let tool = CommandTool::new("echo");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Success(_)));
    }

    #[tokio::test]
    async fn a_tool_without_an_allowed_pattern_rejects_a_command_with_arguments() {
        let tool = CommandTool::new("echo");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo hello" }), &ctx)
            .await
            .unwrap();
        let content = result.content();
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("is not allowed by tool 'echo'"));
    }

    #[tokio::test]
    async fn a_command_matching_no_allowed_pattern_is_rejected() {
        let tool = CommandTool::new("echo").allow("echo *");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "rm -rf /" }), &ctx)
            .await
            .unwrap();
        let content = result.content();
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("Allowed: `echo *`."));
    }

    #[tokio::test]
    async fn every_allowed_pattern_is_accepted() {
        let tool = CommandTool::new("echo")
            .allow("echo one*")
            .allow("echo two*");
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
        let tool = CommandTool::new("echo")
            .allow("echo *")
            .deny("echo secret*");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo secret" }), &ctx)
            .await
            .unwrap();
        let content = result.content();
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("denied pattern 'echo secret*'"));
    }

    #[tokio::test]
    async fn a_denied_pattern_overrules_the_bare_command() {
        let tool = CommandTool::new("echo").deny("echo");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Error(_)));
    }

    /// Run `command` through a tool whose pattern matches the whole line, so a
    /// refusal can only come from the one-command rule, and prove the second
    /// command never reached the disk.
    async fn refuses_chaining(command: &str) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let tool = CommandTool::new("touch").allow("touch *");

        let result = tool
            .call(serde_json::json!({ "command": command }), &ctx)
            .await
            .unwrap();

        let content = result.content();
        assert!(
            matches!(result, ToolResult::Error(_)),
            "{command:?} should be refused, got {content}"
        );
        assert!(
            !dir.path().join("chained.txt").exists(),
            "{command:?} reached the disk"
        );
    }

    #[tokio::test]
    async fn a_single_command_runs() {
        // The control for every refusal below: without it they would all pass
        // on a tool that refuses everything.
        let dir = crate::test_util::TempDir::new().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        let result = CommandTool::new("touch")
            .allow("touch *")
            .call(serde_json::json!({ "command": "touch chained.txt" }), &ctx)
            .await
            .unwrap();

        assert!(matches!(result, ToolResult::Success(_)));
        assert!(dir.path().join("chained.txt").exists());
    }

    #[tokio::test]
    async fn a_chained_command_is_refused() {
        refuses_chaining("touch a.txt && touch chained.txt").await;
    }

    #[tokio::test]
    async fn a_substituted_command_is_refused() {
        refuses_chaining("touch $(echo chained.txt)").await;
    }

    #[tokio::test]
    async fn a_command_ending_inside_a_quote_is_refused() {
        refuses_chaining("echo \"hi && touch chained.txt").await;
    }

    #[tokio::test]
    async fn an_operator_inside_quotes_is_one_command() {
        let ctx = test_tool_context();
        let result = CommandTool::new("echo")
            .allow("echo *")
            .call(serde_json::json!({ "command": "echo \"a && b\"" }), &ctx)
            .await
            .unwrap();

        let content = result.content();
        assert!(matches!(result, ToolResult::Success(_)), "got {content}");
        assert!(content.contains("a && b"));
    }

    #[tokio::test]
    async fn a_quoted_argument_reaches_the_program_as_one_word() {
        // Without a shell the program is handed an argument list, so a name
        // holding a space has to survive as one entry rather than two.
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("two words.txt"), "x").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        let result = CommandTool::new("ls")
            .allow("ls *")
            .call(
                serde_json::json!({ "command": "ls -1 \"two words.txt\"" }),
                &ctx,
            )
            .await
            .unwrap();

        let content = result.content();
        assert!(matches!(result, ToolResult::Success(_)), "got {content}");
        assert!(content.contains("two words.txt"), "got {content}");
    }

    #[tokio::test]
    async fn an_absolute_program_path_runs_when_a_pattern_allows_it() {
        let ctx = test_tool_context();
        let result = CommandTool::new("echo")
            .allow("/bin/echo *")
            .call(serde_json::json!({ "command": "/bin/echo hi" }), &ctx)
            .await
            .unwrap();

        let content = result.content();
        assert!(matches!(result, ToolResult::Success(_)), "got {content}");
        assert!(content.contains("hi"));
    }

    #[tokio::test]
    async fn extra_whitespace_does_not_escape_a_denied_pattern() {
        // Asserted against echo: a regression here on a real `git push` would
        // push from the checkout instead of failing the assertion.
        let tool = CommandTool::new("echo").allow("echo *").deny("echo push*");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo  push --force" }), &ctx)
            .await
            .unwrap();
        let content = result.content();
        assert!(
            content.contains("denied pattern 'echo push*'"),
            "got {content}"
        );
    }

    #[tokio::test]
    async fn a_denied_flag_is_refused_wherever_it_sits() {
        let tool = CommandTool::new("ls").allow("ls *").deny_flag("-l");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "ls -a -l" }), &ctx)
            .await
            .unwrap();
        let content = result.content();
        assert!(content.contains("denied flag '-l'"), "got {content}");
    }

    #[tokio::test]
    async fn a_denied_flag_catches_the_value_it_carries() {
        let tool = CommandTool::new("git").allow("git *").deny_flag("--format");
        let ctx = test_tool_context();
        let result = tool
            .call(
                serde_json::json!({ "command": "git log --format=%H" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn a_denied_short_flag_catches_a_value_written_against_it() {
        // `-o` carrying its value is how a single allowed ssh reaches arbitrary
        // code, so the rule cannot depend on the value being letters. Asserted
        // against echo: a regression here on a real ssh would hang on the
        // network until the tool timeout instead of failing the assertion.
        let tool = CommandTool::new("echo").allow("echo *").deny_flag("-o");
        let ctx = test_tool_context();
        let result = tool
            .call(
                serde_json::json!({ "command": "echo -oProxyCommand=/bin/sh host" }),
                &ctx,
            )
            .await
            .unwrap();
        let content = result.content();
        assert!(
            content.contains("denied flag '-oProxyCommand=/bin/sh'"),
            "got {content}"
        );
    }

    #[tokio::test]
    async fn a_denied_short_flag_leaves_the_long_spelling_alone() {
        // Guarding both spellings takes two calls, so the tool must not pretend
        // one covers the other.
        let tool = CommandTool::new("echo").allow("echo *").deny_flag("-f");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo --force" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Success(_)));
    }

    #[tokio::test]
    async fn a_denied_long_flag_leaves_the_short_spelling_alone() {
        let tool = CommandTool::new("echo")
            .allow("echo *")
            .deny_flag("--force");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo -f" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Success(_)));
    }

    #[tokio::test]
    async fn a_denied_letter_reaches_into_a_cluster() {
        let tool = CommandTool::new("echo").allow("echo *").deny_flag("-f");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo -rf" }), &ctx)
            .await
            .unwrap();
        assert!(result.content().contains("denied flag '-rf'"));
    }

    #[tokio::test]
    async fn a_denied_cluster_catches_only_that_spelling() {
        // Naming several letters reads as the cluster, not as a rule per
        // letter, so `-r` on its own is untouched by it.
        let tool = CommandTool::new("echo").allow("echo *").deny_flag("-rf");
        let ctx = test_tool_context();

        let refused = tool
            .clone()
            .call(serde_json::json!({ "command": "echo -rf" }), &ctx)
            .await
            .unwrap();
        assert!(refused.content().contains("denied flag '-rf'"));

        let allowed = tool
            .call(serde_json::json!({ "command": "echo -r" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(allowed, ToolResult::Success(_)));
    }

    #[tokio::test]
    async fn an_absolute_path_is_refused_by_a_pattern_naming_the_program() {
        // Fail closed: the pattern matches what runs, so an operator wanting the
        // absolute path allows it.
        let tool = CommandTool::new("echo").allow("echo *");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "/bin/echo hi" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn an_environment_assignment_is_refused() {
        let tool = CommandTool::new("echo").allow("echo *");
        let ctx = test_tool_context();
        let result = tool
            .call(
                serde_json::json!({ "command": "LD_PRELOAD=./x echo hi" }),
                &ctx,
            )
            .await
            .unwrap();
        let content = result.content();
        assert!(content.contains("environment variable"), "got {content}");
    }

    #[tokio::test]
    async fn a_missing_program_names_itself_in_the_error() {
        let ctx = test_tool_context();
        let result = CommandTool::new("nonexistent_command_xyz")
            .call(
                serde_json::json!({ "command": "nonexistent_command_xyz" }),
                &ctx,
            )
            .await
            .unwrap();
        let content = result.content();
        assert!(content.contains("nonexistent_command_xyz"), "got {content}");
    }

    #[test]
    fn a_description_lists_the_denied_flags() {
        let tool = CommandTool::new("git").allow("git *").deny_flag("--force");
        assert!(ToolLike::description(&tool).contains("Denied flags: `--force`."));
    }

    #[test]
    #[should_panic(expected = "is not a flag")]
    fn a_deny_flag_rule_that_is_not_a_flag_is_refused_at_construction() {
        // Silently accepted, the rule would sit inert and deny nothing.
        let _ = CommandTool::new("git").deny_flag("force");
    }

    #[test]
    #[should_panic(expected = "is not a flag")]
    fn a_deny_flag_rule_naming_a_number_is_refused_at_construction() {
        let _ = CommandTool::new("head").deny_flag("-5");
    }

    #[tokio::test]
    async fn a_denied_flag_after_a_double_dash_is_an_operand() {
        // The getopt convention: after `--` the token names a file, so the
        // rule must not refuse it.
        let tool = CommandTool::new("echo")
            .allow("echo *")
            .deny_flag("--force");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo -- --force" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Success(_)));
    }

    #[tokio::test]
    async fn a_quoted_multiword_argument_satisfies_the_same_pattern_as_words() {
        // The documented limit of allow patterns: quoting is gone by the time
        // the pattern is asked, so the program receives one argument the
        // pattern read as two.
        let tool = CommandTool::new("echo").allow("echo a b");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "echo \"a b\"" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Success(_)));
    }

    #[tokio::test]
    async fn a_program_differing_in_case_is_refused() {
        // A case-insensitive filesystem finds ECHO for echo, so the pattern
        // failing closed is what keeps it from widening the allow list.
        let tool = CommandTool::new("echo").allow("echo *");
        let ctx = test_tool_context();
        let result = tool
            .call(serde_json::json!({ "command": "ECHO hi" }), &ctx)
            .await
            .unwrap();
        assert!(matches!(result, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn a_denied_command_never_runs() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let tool = CommandTool::new("touch")
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
