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

        // `name` is read ahead of the strict pass, so every refusal below can
        // say whose definition is broken.
        let name = frontmatter_value(front, "name").unwrap_or_else(|| {
            panic!(
                "tool definition missing `name` in frontmatter, starts {:?}",
                first_line(front)
            )
        });

        let mut concurrent: Option<bool> = None;
        for line in front.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once(':') else {
                panic!("tool `{name}`: frontmatter line {line:?} is not `key: value`");
            };
            match key.trim() {
                "name" => {}
                "concurrent" => {
                    concurrent = Some(match value.trim() {
                        "true" => true,
                        "false" => false,
                        other => panic!(
                            "tool `{name}`: `concurrent` must be `true` or `false`, got `{other}`"
                        ),
                    });
                }
                unknown => panic!("tool `{name}`: unknown frontmatter key `{unknown}`"),
            }
        }

        ToolFile {
            input_schema: compile_schema(schema, &name),
            concurrent: concurrent
                .unwrap_or_else(|| panic!("tool `{name}`: frontmatter missing `concurrent`")),
            name,
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
/// block is missing or unterminated, quoting how the document starts, since no
/// name exists yet to say whose it is.
fn split_frontmatter(markdown: &str) -> (&str, &str) {
    let rest = markdown.strip_prefix("---\n").unwrap_or_else(|| {
        panic!(
            "tool definition must open with `---` frontmatter, starts {:?}",
            first_line(markdown)
        )
    });
    rest.split_once("\n---\n").unwrap_or_else(|| {
        panic!(
            "tool definition has an unterminated `---` frontmatter block, starts {:?}",
            first_line(rest)
        )
    })
}

/// The value of `key` in the frontmatter, or `None` when no line declares it.
fn frontmatter_value(front: &str, key: &str) -> Option<String> {
    front.lines().find_map(|line| {
        let (written, value) = line.split_once(':')?;
        (written.trim() == key).then(|| value.trim().to_string())
    })
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
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
    #[should_panic(expected = "tool `demo`: unknown frontmatter key `concurent`")]
    fn an_unknown_frontmatter_key_fails_the_build_naming_the_tool() {
        // Silently skipping the misspelling would leave `concurrent` at its
        // default and the mistake undiscovered.
        ToolFile::parse(
            "\
---
name: demo
concurent: true
---

Body.
",
            r#"{ "type": "object" }"#,
        );
    }

    #[test]
    #[should_panic(expected = "tool `demo`: frontmatter line \"concurrent\" is not `key: value`")]
    fn a_frontmatter_line_without_a_colon_fails_the_build_naming_the_tool() {
        ToolFile::parse(
            "\
---
name: demo
concurrent
---

Body.
",
            r#"{ "type": "object" }"#,
        );
    }

    #[test]
    #[should_panic(expected = "tool `demo`: `concurrent` must be `true` or `false`, got `yes`")]
    fn a_concurrent_value_that_is_not_a_boolean_fails_the_build() {
        ToolFile::parse(
            "\
---
name: demo
concurrent: yes
---

Body.
",
            r#"{ "type": "object" }"#,
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
