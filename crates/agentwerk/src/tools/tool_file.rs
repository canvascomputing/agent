//! Reads a tool's definition out of two files: the markdown the model is told,
//! and the JSON Schema document for what the tool accepts.

use crate::schemas::Schema;

/// Parsed view of a tool's `.tool.md` file and its sibling `.schema.json`.
/// `description` is the prose body verbatim (already the markdown the model
/// sees); `input_schema` is the compiled schema document.
#[derive(Debug)]
pub(crate) struct ToolFile {
    pub(crate) name: String,
    pub(crate) concurrent: bool,
    pub(crate) input_schema: Schema,
    description: String,
}

impl ToolFile {
    /// Parse a `.tool.md` definition and the JSON Schema document that sits
    /// beside it. Panics on a malformed file or a schema that does not
    /// compile: the definitions are compile-time assets included via
    /// `include_str!`, not runtime input, so a broken one should fail the
    /// build, not a request.
    pub(crate) fn parse(definition: &str, schema: &str) -> Self {
        let (front, body) = split_frontmatter(definition);

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
        let name = name.expect("tool definition missing `name` in frontmatter");

        ToolFile {
            input_schema: compile_schema(schema, &name),
            name,
            concurrent: concurrent.expect("tool definition missing `concurrent` in frontmatter"),
            description: body.trim().to_string(),
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

/// Compile the schema document beside `name`'s definition. Panics on a
/// document `Schema::new` refuses, for the reason [`ToolFile::parse`] panics
/// on a malformed file: a definition is a compile-time asset, so a broken one
/// should fail the build rather than leave `name` running against a weaker
/// check.
fn compile_schema(schema: &str, name: &str) -> Schema {
    let document = serde_json::from_str(schema).unwrap_or_else(|error| {
        panic!("tool `{name}` declares a schema that is not JSON: {error}")
    });
    Schema::new(document).unwrap_or_else(|error| {
        panic!("tool `{name}` declares a schema that does not compile: {error}")
    })
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
";

    const FIXTURE_SCHEMA: &str = r#"
{
  "type": "object",
  "properties": { "x": { "type": "string" } },
  "required": ["x"]
}
"#;

    #[test]
    fn parses_frontmatter_name_and_concurrent() {
        let tf = ToolFile::parse(FIXTURE, FIXTURE_SCHEMA);
        assert_eq!(tf.name, "fixture_tool");
        assert!(tf.concurrent);
    }

    #[test]
    fn description_is_the_prose_body() {
        let tf = ToolFile::parse(FIXTURE, FIXTURE_SCHEMA);
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
    fn parses_the_input_schema_from_the_sibling_document() {
        let tf = ToolFile::parse(FIXTURE, FIXTURE_SCHEMA);
        let document = tf.input_schema.get_raw_schema();
        assert_eq!(document["properties"]["x"]["type"], "string");
        assert_eq!(document["required"][0], "x");
    }

    #[test]
    #[should_panic(expected = "tool `broken` declares a schema that does not compile")]
    fn a_schema_the_compiler_refuses_fails_the_build() {
        ToolFile::parse(
            "\
---
name: broken
concurrent: false
---

Body.
",
            r#"{ "uniqueItems": true }"#,
        );
    }

    #[test]
    #[should_panic(expected = "tool `broken` declares a schema that is not JSON")]
    fn a_schema_that_is_not_json_fails_the_build_naming_the_tool() {
        ToolFile::parse(
            "\
---
name: broken
concurrent: false
---

Body.
",
            "{ not json",
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
";
        let tf = ToolFile::parse(md, r#"{ "type": "object" }"#);
        assert_eq!(tf.name, "minimal");
        assert!(!tf.concurrent);
        assert_eq!(tf.render_markdown(), "Body.");
    }
}
