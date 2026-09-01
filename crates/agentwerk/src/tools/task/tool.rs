//! The model-facing tool for reading and editing tasks in a Werk.

use super::super::tool::{Tool, ToolContext};
use super::dispatch;

/// `task`, `result`, `list`, `create`, `edit` in one tool.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::TaskTool;
///
/// Agent::new().tool(TaskTool);
/// ```
pub struct TaskTool;

impl From<TaskTool> for Tool {
    fn from(_: TaskTool) -> Tool {
        Tool::new("task")
            .description(include_str!("task.tool.md"))
            .schema(include_str!("task.schema.json"))
            .handler_with_context(|args: super::TaskArgs, ctx: ToolContext| async move {
                dispatch(args, &ctx)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        // The schema and `TaskArgs` both describe the shape. The examples
        // are where they are held to the same one.
        let document = Tool::from(TaskTool)
            .get_input_schema()
            .get_raw_schema()
            .clone();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<super::super::TaskArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }

    #[test]
    fn the_schema_advertises_exactly_the_arguments_dispatch_reads() {
        // The schema is parsed from markdown at runtime, so a property the
        // model is told about but `dispatch` never reads is silently dropped
        // rather than caught by the compiler.
        let tool = Tool::from(TaskTool);
        let schema = tool.get_input_schema();
        let advertised: BTreeSet<&str> = schema.get_raw_schema()["properties"]
            .as_object()
            .expect("properties is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            advertised,
            BTreeSet::from(["action", "aql", "id", "label", "task"]),
        );
    }
}
