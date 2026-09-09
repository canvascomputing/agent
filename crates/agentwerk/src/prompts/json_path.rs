//! Parses and evaluates the navigation subset used by result templates.

use std::fmt;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JsonPath {
    steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Field(String),
    Index(isize),
    Slice {
        start: Option<isize>,
        stop: Option<isize>,
        step: isize,
    },
    ArrayWildcard,
    ObjectWildcard,
    Flatten,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JsonPathError {
    message: String,
}

impl JsonPathError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for JsonPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JsonPathError {}

impl JsonPath {
    pub(super) fn parse(source: &str) -> Result<Self, JsonPathError> {
        Parser::new(source.trim()).parse()
    }

    pub(super) fn evaluate(&self, value: &Value) -> Value {
        evaluate(value, &self.steps)
    }
}

struct Parser<'a> {
    source: &'a str,
    byte_offset: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            byte_offset: 0,
        }
    }

    fn parse(mut self) -> Result<JsonPath, JsonPathError> {
        if self.source.is_empty() {
            return Err(JsonPathError::new("JSON path cannot be empty"));
        }

        let mut steps = vec![self.initial_step()?];
        while let Some(character) = self.peek() {
            match character {
                '.' => {
                    self.advance('.');
                    steps.push(self.field_step()?);
                }
                '[' => steps.push(self.bracket_step()?),
                _ => return Err(self.unsupported()),
            }
        }

        Ok(JsonPath { steps })
    }

    fn initial_step(&mut self) -> Result<Step, JsonPathError> {
        match self.peek() {
            Some('"') => self.quoted_field(),
            Some('*') => {
                self.advance('*');
                Ok(Step::ObjectWildcard)
            }
            Some('[') => self.bracket_step(),
            Some(character) if is_identifier_start(character) => self.identifier(),
            _ => Err(self.unsupported()),
        }
    }

    fn field_step(&mut self) -> Result<Step, JsonPathError> {
        match self.peek() {
            Some('"') => self.quoted_field(),
            Some('*') => {
                self.advance('*');
                Ok(Step::ObjectWildcard)
            }
            Some(character) if is_identifier_start(character) => self.identifier(),
            _ => Err(JsonPathError::new(format!(
                "expected a JSON path field at byte {}",
                self.byte_offset
            ))),
        }
    }

    fn identifier(&mut self) -> Result<Step, JsonPathError> {
        let start = self.byte_offset;
        while let Some(character) = self.peek() {
            if !is_identifier_continue(character) {
                break;
            }
            self.advance(character);
        }

        Ok(Step::Field(
            self.source[start..self.byte_offset].to_string(),
        ))
    }

    fn quoted_field(&mut self) -> Result<Step, JsonPathError> {
        let start = self.byte_offset;
        self.advance('"');
        let mut escaped = false;
        while let Some(character) = self.next_character() {
            if escaped {
                escaped = false;
                continue;
            }
            if character == '\\' {
                escaped = true;
                continue;
            }
            if character == '"' {
                let encoded = &self.source[start..self.byte_offset];
                let field = serde_json::from_str::<String>(encoded)
                    .map_err(|_| JsonPathError::new("invalid quoted JSON path field"))?;
                if field.is_empty() {
                    return Err(JsonPathError::new(
                        "quoted JSON path fields cannot be empty",
                    ));
                }
                return Ok(Step::Field(field));
            }
        }
        Err(JsonPathError::new("unclosed quoted JSON path field"))
    }

    fn bracket_step(&mut self) -> Result<Step, JsonPathError> {
        self.advance('[');
        let start = self.byte_offset;
        let Some(relative_end) = self.source[start..].find(']') else {
            return Err(JsonPathError::new("unclosed JSON path bracket"));
        };
        let end = start + relative_end;
        let body = &self.source[start..end];
        self.byte_offset = end + ']'.len_utf8();

        match body {
            "" => Ok(Step::Flatten),
            "*" => Ok(Step::ArrayWildcard),
            body if body.contains(':') => parse_slice(body),
            body => parse_index(body),
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.byte_offset..].chars().next()
    }

    fn advance(&mut self, character: char) {
        self.byte_offset += character.len_utf8();
    }

    fn next_character(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.advance(character);
        Some(character)
    }

    fn unsupported(&self) -> JsonPathError {
        JsonPathError::new(format!(
            "unsupported JSON path syntax at byte {}",
            self.byte_offset
        ))
    }
}

fn parse_index(source: &str) -> Result<Step, JsonPathError> {
    let index = parse_integer(source, "array index")?;
    Ok(Step::Index(index))
}

fn parse_slice(source: &str) -> Result<Step, JsonPathError> {
    let parts = source.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return Err(JsonPathError::new("invalid JSON path slice"));
    }
    let start = parse_optional_integer(parts[0], "slice bound")?;
    let stop = parse_optional_integer(parts[1], "slice bound")?;
    let step = match parts.get(2).copied().filter(|part| !part.is_empty()) {
        Some(step) => parse_integer(step, "slice step")?,
        None => 1,
    };
    if step == 0 {
        return Err(JsonPathError::new("JSON path slice step cannot be zero"));
    }
    Ok(Step::Slice { start, stop, step })
}

fn parse_optional_integer(source: &str, description: &str) -> Result<Option<isize>, JsonPathError> {
    if source.is_empty() {
        return Ok(None);
    }
    parse_integer(source, description).map(Some)
}

fn parse_integer(source: &str, description: &str) -> Result<isize, JsonPathError> {
    let digits = source.strip_prefix('-').unwrap_or(source);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(JsonPathError::new(format!(
            "invalid JSON path {description}"
        )));
    }
    source.parse::<isize>().map_err(|_| {
        JsonPathError::new(format!(
            "JSON path {description} is outside the supported range"
        ))
    })
}

fn is_identifier_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_'
}

fn is_identifier_continue(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn evaluate(value: &Value, steps: &[Step]) -> Value {
    let Some((step, remaining_steps)) = steps.split_first() else {
        return value.clone();
    };

    match step {
        Step::Field(field) => {
            let Some(value) = value.as_object().and_then(|fields| fields.get(field)) else {
                return Value::Null;
            };
            evaluate(value, remaining_steps)
        }
        Step::Index(index) => {
            let Some(values) = value.as_array() else {
                return Value::Null;
            };
            let Some(position) = array_index(values.len(), *index) else {
                return Value::Null;
            };
            evaluate(&values[position], remaining_steps)
        }
        Step::Slice { start, stop, step } => {
            let Some(values) = value.as_array() else {
                return Value::Null;
            };
            let selected = slice(values, *start, *stop, *step);
            evaluate_each(selected, remaining_steps)
        }
        Step::ArrayWildcard => {
            let Some(values) = value.as_array() else {
                return Value::Null;
            };
            evaluate_each(values.iter(), remaining_steps)
        }
        Step::ObjectWildcard => {
            let Some(fields) = value.as_object() else {
                return Value::Null;
            };
            evaluate_each(fields.values(), remaining_steps)
        }
        Step::Flatten => {
            let Some(values) = value.as_array() else {
                return Value::Null;
            };
            evaluate_each(flatten_once(values), remaining_steps)
        }
    }
}

fn evaluate_each<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    remaining_steps: &[Step],
) -> Value {
    let selected = values
        .into_iter()
        .filter_map(|value| {
            let selected = evaluate(value, remaining_steps);
            (remaining_steps.is_empty() || !selected.is_null()).then_some(selected)
        })
        .collect();
    Value::Array(selected)
}

fn flatten_once(values: &[Value]) -> Vec<&Value> {
    let mut flattened = Vec::new();
    for value in values {
        match value {
            Value::Array(nested) => flattened.extend(nested),
            value => flattened.push(value),
        }
    }
    flattened
}

fn array_index(length: usize, index: isize) -> Option<usize> {
    let length = length as i128;
    let index = index as i128;
    let index = if index < 0 { length + index } else { index };
    if !(0..length).contains(&index) {
        return None;
    }
    usize::try_from(index).ok()
}

fn slice(values: &[Value], start: Option<isize>, stop: Option<isize>, step: isize) -> Vec<&Value> {
    let length = values.len() as i128;
    let step = step as i128;
    let (mut index, stop) = if step > 0 {
        (
            positive_bound(start, length, 0),
            positive_bound(stop, length, length),
        )
    } else {
        (
            negative_bound(start, length, length - 1),
            negative_bound(stop, length, -1),
        )
    };

    let mut selected = Vec::new();
    while (step > 0 && index < stop) || (step < 0 && index > stop) {
        if let Some(value) = usize::try_from(index)
            .ok()
            .and_then(|position| values.get(position))
        {
            selected.push(value);
        }
        index += step;
    }
    selected
}

fn positive_bound(bound: Option<isize>, length: i128, default: i128) -> i128 {
    let Some(bound) = bound else {
        return default;
    };
    let mut bound = bound as i128;
    if bound < 0 {
        bound += length;
    }
    bound.clamp(0, length)
}

fn negative_bound(bound: Option<isize>, length: i128, default: i128) -> i128 {
    let Some(bound) = bound else {
        return default;
    };
    let mut bound = bound as i128;
    if bound < 0 {
        bound += length;
    }
    bound.clamp(-1, length - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn evaluate_path(path: &str, value: Value) -> Value {
        JsonPath::parse(path).unwrap().evaluate(&value)
    }

    #[test]
    fn fields_support_chaining_and_json_quoted_names() {
        let value = json!({
            "company": {"display-name": "Acme"},
            "quote\"field": "escaped",
        });

        assert_eq!(
            evaluate_path(r#"company."display-name""#, value.clone()),
            json!("Acme")
        );
        assert_eq!(evaluate_path(r#""quote\"field""#, value), json!("escaped"));
    }

    #[test]
    fn fields_return_null_when_missing_or_applied_to_another_type() {
        assert_eq!(
            evaluate_path("missing", json!({"name": "Acme"})),
            Value::Null
        );
        assert_eq!(
            evaluate_path("name.first", json!({"name": "Acme"})),
            Value::Null
        );
    }

    #[test]
    fn collection_steps_return_null_for_incompatible_types() {
        for path in ["[0]", "[:2]", "[*]", "[]"] {
            assert_eq!(evaluate_path(path, json!({"name": "Acme"})), Value::Null);
        }
        assert_eq!(evaluate_path("*", json!(["one", "two"])), Value::Null);
    }

    #[test]
    fn indexes_accept_positive_negative_and_chained_positions() {
        let value = json!([["first"], ["last"]]);

        assert_eq!(evaluate_path("[0][0]", value.clone()), json!("first"));
        assert_eq!(evaluate_path("[-1][0]", value), json!("last"));
    }

    #[test]
    fn out_of_range_indexes_return_null() {
        for path in ["[100]", "[-100]"] {
            assert_eq!(evaluate_path(path, json!(["first"])), Value::Null);
        }
    }

    #[test]
    fn slices_follow_python_bounds_and_steps() {
        let value = json!([0, 1, 2, 3]);
        for (path, expected) in [
            ("[0:3]", json!([0, 1, 2])),
            ("[:2]", json!([0, 1])),
            ("[::2]", json!([0, 2])),
            ("[-2:]", json!([2, 3])),
            ("[::-1]", json!([3, 2, 1, 0])),
            ("[3::-1]", json!([3, 2, 1, 0])),
            ("[3:-1:-1]", json!([])),
            ("[3:0:-2]", json!([3, 1])),
            ("[-10:10]", json!([0, 1, 2, 3])),
            ("[10:-10:-1]", json!([3, 2, 1, 0])),
            ("[8:10]", json!([])),
        ] {
            assert_eq!(evaluate_path(path, value.clone()), expected, "{path}");
        }
        assert_eq!(evaluate_path("[::-1]", json!([])), json!([]));
    }

    #[test]
    fn slices_reject_zero_steps() {
        assert_eq!(
            JsonPath::parse("[::0]").unwrap_err().to_string(),
            "JSON path slice step cannot be zero"
        );
    }

    #[test]
    fn array_integers_must_fit_the_supported_range() {
        for path in [
            "[999999999999999999999999]",
            "[-999999999999999999999999]",
            "[999999999999999999999999:]",
            "[:999999999999999999999999]",
            "[::999999999999999999999999]",
        ] {
            assert!(JsonPath::parse(path).is_err(), "{path}");
        }
    }

    #[test]
    fn collection_projections_apply_the_remaining_path_and_omit_nulls() {
        let value = json!([{"name": "one"}, {}, {"name": null}, {"name": "two"}]);

        for (path, expected) in [
            ("[*].name", json!(["one", "two"])),
            ("[1:].name", json!(["two"])),
        ] {
            assert_eq!(evaluate_path(path, value.clone()), expected, "{path}");
        }
    }

    #[test]
    fn object_wildcards_apply_the_remaining_path() {
        let value = json!({
            "one": {"name": "first"},
            "two": {},
            "three": {"name": "last"}
        });
        let mut selected = evaluate_path("*.name", value).as_array().unwrap().clone();
        selected.sort_by_key(Value::to_string);

        assert_eq!(selected, vec![json!("first"), json!("last")]);
    }

    #[test]
    fn flatten_merges_one_level_then_applies_the_remaining_path() {
        let value = json!([[{"name": "one"}], {"name": "two"}, [[{"name": "deep"}]]]);

        assert_eq!(evaluate_path("[].name", value), json!(["one", "two"]));
    }

    #[test]
    fn collection_steps_without_a_following_path_preserve_nulls() {
        let value = json!([[1, null], null, [2]]);

        assert_eq!(
            evaluate_path("[]", value.clone()),
            json!([1, null, null, 2])
        );
        assert_eq!(evaluate_path("[*]", value), json!([[1, null], null, [2]]));
    }

    #[test]
    fn unsupported_jmespath_features_are_rejected() {
        for path in [
            "items[?active]",
            "foo == bar",
            "foo || bar",
            "length(items)",
            "[foo,bar]",
            "{foo: foo}",
            "'literal'",
            "`1`",
            "@",
            "foo | bar",
        ] {
            assert!(JsonPath::parse(path).is_err(), "{path}");
        }
    }

    #[test]
    fn malformed_json_paths_are_rejected() {
        for path in [
            "",
            "foo.",
            "foo..bar",
            "foo bar",
            "\"",
            "\"\"",
            "[",
            "[one]",
            "[1:2:3:4]",
            r#""\q""#,
        ] {
            assert!(JsonPath::parse(path).is_err(), "{path}");
        }
    }
}
