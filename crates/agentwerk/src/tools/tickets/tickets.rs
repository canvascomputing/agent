//! Lets an agent read the ticket queue and create or edit tickets in it.

use super::super::tool::{Tool, ToolContext};
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

impl From<TicketsTool> for Tool {
    fn from(_: TicketsTool) -> Tool {
        Tool::from_tool_file(
            include_str!("tickets.tool.md"),
            include_str!("tickets.schema.json"),
            |args: super::TicketsArgs, ctx: ToolContext| async move { dispatch(args, &ctx) },
        )
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
        let document = Tool::from(TicketsTool)
            .input_schema()
            .get_raw_schema()
            .clone();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<super::super::TicketsArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }

    #[test]
    fn the_schema_advertises_exactly_the_arguments_dispatch_reads() {
        // The schema is parsed from markdown at runtime, so a property the
        // model is told about but `dispatch` never reads is silently dropped
        // rather than caught by the compiler.
        let tool = Tool::from(TicketsTool);
        let schema = tool.input_schema();
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
