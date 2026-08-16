//! Lets an agent read the ticket queue and create or edit tickets in it.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use crate::schemas::Schema;

use super::super::tool::{ToolContext, ToolLike, ToolResult};
use super::super::tool_file::ToolFile;
use super::dispatch;

/// `ticket`, `result`, `list`, `search`, `create`, `edit` in one tool.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::TicketsTool;
///
/// Agent::new().tool(TicketsTool);
/// ```
pub struct TicketsTool;

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("tickets.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

impl ToolLike for TicketsTool {
    type Args = super::TicketsArgs;

    fn name(&self) -> &str {
        &tool_file().name
    }

    fn description(&self) -> &str {
        description()
    }

    fn input_schema(&self) -> Schema {
        tool_file().input_schema.clone()
    }

    fn is_concurrent(&self) -> bool {
        tool_file().concurrent
    }

    fn call<'a>(
        &'a self,
        args: super::TicketsArgs,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move { dispatch(args, ctx) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        // The schema and `TicketsArgs` both describe the shape. The examples
        // are where they are held to the same one.
        let document = tool_file().input_schema.get_raw_schema().clone();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<crate::tools::TicketsArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }

    #[test]
    fn the_schema_advertises_exactly_the_arguments_dispatch_reads() {
        // The schema is parsed from markdown at runtime, so a property the
        // model is told about but `dispatch` never reads is silently dropped
        // rather than caught by the compiler.
        let schema = TicketsTool.input_schema();
        let advertised: BTreeSet<&str> = schema.get_raw_schema()["properties"]
            .as_object()
            .expect("properties is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            advertised,
            BTreeSet::from(["action", "key", "status", "label", "query", "task"]),
        );
    }
}
