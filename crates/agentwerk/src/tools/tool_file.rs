//! Reads a tool's whole definition out of one markdown file: its name, what the
//! model is told it does, and what it accepts.

use serde_json::Value;

use crate::schemas::Schema;

/// Parsed view of a tool's `.tool.md` file. `description` is the prose body
/// verbatim (already the markdown the model sees); `input_schema` is the
/// compiled `## Schema` fence. Authors write markdown; the framework only
/// splits it into these fields.
#[derive(Debug)]
pub(crate) struct ToolFile {
    pub(crate) name: String,
    pub(crate) concurrent: bool,
    pub(crate) input_schema: Schema,
    description: String,
}

impl ToolFile {
    /// Parse a `.tool.md` document. Panics on a malformed file: the
    /// definitions are compile-time assets included via `include_str!`, not
    /// runtime input, so a broken one should fail the build, not a request.
    pub(crate) fn parse(markdown: &str) -> Self {
        let (front, body) = split_frontmatter(markdown);

        let mut name: Option<String> = None;
        let mut concurrent: Option<bool> = None;
        for line in front.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            match key.trim() {
                "name" => name = Some(value.trim().to_string()),
                "concurrent" => concurrent = Some(value.trim() == "true"),
                _ => {}
            }
        }

        let (description, schema_section) = body
            .split_once("\n## Schema")
            .expect("tool definition missing `## Schema` section");
        let name = name.expect("tool definition missing `name` in frontmatter");

        ToolFile {
            input_schema: compile_json_fence(schema_section, &name),
            name,
            concurrent: concurrent.expect("tool definition missing `concurrent` in frontmatter"),
            description: description.trim().to_string(),
        }
    }

    /// The prose body shown to the model. Named for the format it returns so
    /// the `ToolLike` impls that cache it read the same as before the markdown
    /// migration.
    pub(crate) fn render_markdown(&self) -> String {
        self.description.clone()
    }
}

/// Split a leading `---` frontmatter block from the body. Panics when the
/// block is missing or unterminated.
fn split_frontmatter(markdown: &str) -> (&str, &str) {
    let rest = markdown
        .strip_prefix("---\n")
        .expect("tool definition must open with `---` frontmatter");
    rest.split_once("\n---\n")
        .expect("tool definition has an unterminated `---` frontmatter block")
}

/// Compile the first ` ```json ` fence in `section`. Panics on a document
/// `Schema::new` refuses, for the reason [`ToolFile::parse`] panics on a
/// malformed file: a definition is a compile-time asset, so a broken one should
/// fail the build rather than leave `name` running against a weaker check.
fn compile_json_fence(section: &str, name: &str) -> Schema {
    Schema::new(parse_json_fence(section)).unwrap_or_else(|error| {
        panic!("tool `{name}` declares a schema that does not compile: {error}")
    })
}

/// Extract and parse the first ` ```json ` fence in `section`.
fn parse_json_fence(section: &str) -> Value {
    let start = section
        .find("```json")
        .expect("`## Schema` section missing a ```json fence");
    let after = section[start + "```json".len()..].trim_start_matches(['\r', '\n']);
    let end = after
        .find("```")
        .expect("`## Schema` section has an unterminated ```json fence");
    serde_json::from_str(after[..end].trim()).expect("invalid tool input_schema JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
---
name: fixture_tool
concurrent: true
---

First sentence. Second sentence.

- A constraint.
- Another constraint.

## When NOT to use

- Use X instead.

## Schema

```json
{
  \"type\": \"object\",
  \"properties\": { \"x\": { \"type\": \"string\" } },
  \"required\": [\"x\"]
}
```
";

    #[test]
    fn parses_frontmatter_name_and_concurrent() {
        let tf = ToolFile::parse(FIXTURE);
        assert_eq!(tf.name, "fixture_tool");
        assert!(tf.concurrent);
    }

    #[test]
    fn description_is_the_prose_body_without_the_schema_section() {
        let tf = ToolFile::parse(FIXTURE);
        let expected = "First sentence. Second sentence.\n\
                        \n\
                        - A constraint.\n\
                        - Another constraint.\n\
                        \n\
                        ## When NOT to use\n\
                        \n\
                        - Use X instead.";
        assert_eq!(tf.render_markdown(), expected);
    }

    #[test]
    fn parses_the_input_schema_from_the_json_fence() {
        let tf = ToolFile::parse(FIXTURE);
        let document = tf.input_schema.get_raw_schema();
        assert_eq!(document["properties"]["x"]["type"], "string");
        assert_eq!(document["required"][0], "x");
    }

    #[test]
    #[should_panic(expected = "tool `broken` declares a schema that does not compile")]
    fn a_fence_the_schema_compiler_refuses_fails_the_build() {
        ToolFile::parse(
            "\
---
name: broken
concurrent: false
---

Body.

## Schema

```json
{ \"uniqueItems\": true }
```
",
        );
    }

    #[test]
    fn concurrent_false_round_trips() {
        let md = "\
---
name: minimal
concurrent: false
---

Body.

## Schema

```json
{ \"type\": \"object\" }
```
";
        let tf = ToolFile::parse(md);
        assert_eq!(tf.name, "minimal");
        assert!(!tf.concurrent);
        assert_eq!(tf.render_markdown(), "Body.");
    }
}
