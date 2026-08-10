//! Constrains the result an agent produces for a ticket.
//!
//! The supported subset of JSON Schema is deliberately small: structure, the
//! scalar bounds the model APIs do not enforce, the logical and conditional
//! keywords, and `pattern`. Anything outside it is rejected when the schema is
//! parsed, so you learn the limit instead of trusting a constraint that never
//! ran. `pattern` uses the `regex` crate, which has no backreferences,
//! lookahead, or lookbehind.
//!
//! ```
//! use agentwerk::schemas::Schema;
//! use serde_json::json;
//!
//! let schema = Schema::new(json!({
//!     "type": "object",
//!     "properties": { "name": { "type": "string", "minLength": 1 } },
//!     "required": ["name"],
//! }))
//! .unwrap();
//!
//! // A conforming value comes back unchanged.
//! let value = json!({ "name": "Ada" });
//! assert_eq!(schema.validate(value.clone()).unwrap(), value);
//!
//! // Otherwise every problem is reported as a violation.
//! let violations = schema.validate(json!({})).unwrap_err();
//! assert!(violations.iter().any(|v| v.message.contains("name")));
//! ```

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Number, Value};

mod store;

pub use store::SchemaStore;

/// A `Schema` constrains the result an agent produces for a ticket. A violation
/// triggers a retry until `max_schema_retries` is exhausted.
///
/// Build one with [`Schema::new`]. Copying it is cheap, and validating
/// changes nothing.
#[derive(Clone)]
pub struct Schema {
    inner: Arc<SchemaBody>,
}

struct SchemaBody {
    compiled: Node,
    raw_document: Value,
}

impl Schema {
    /// Create a schema.
    ///
    /// A malformed document or an unsupported keyword comes back as a
    /// [`SchemaParseError`] and never counts against the retry budget: it is a
    /// mistake in your code, not in what an agent produced.
    ///
    /// ```
    /// use agentwerk::schemas::Schema;
    /// use serde_json::json;
    ///
    /// assert!(Schema::new(json!({ "type": "string", "pattern": "^[a-z]+$" })).is_ok());
    ///
    /// // A keyword outside the supported set is rejected up front.
    /// let err = Schema::new(json!({ "uniqueItems": true })).unwrap_err();
    /// assert!(err.message.contains("unsupported keyword"));
    /// ```
    pub fn new(document: Value) -> Result<Self, SchemaParseError> {
        let compiled = compile(&document, "")?;
        Ok(Self {
            inner: Arc::new(SchemaBody {
                compiled,
                raw_document: document,
            }),
        })
    }

    /// Validate content and give back the value to keep.
    ///
    /// A value that satisfies the schema comes back unchanged. A value an agent
    /// wrote as a JSON string is decoded once and kept when the decoded form
    /// satisfies the schema. Otherwise the original violations are reported, so
    /// a retry points at the real problem rather than at the decoding.
    ///
    /// ```
    /// use agentwerk::schemas::Schema;
    /// use serde_json::json;
    ///
    /// let schema = Schema::new(json!({ "type": "object", "required": ["status"] })).unwrap();
    ///
    /// // A string an agent double-encoded is decoded once, then validated.
    /// let decoded = schema.validate(json!("{\"status\": \"done\"}")).unwrap();
    /// assert_eq!(decoded, json!({ "status": "done" }));
    ///
    /// // A failure carries the instance path of each problem.
    /// let violations = schema.validate(json!({})).unwrap_err();
    /// assert_eq!(violations[0].instance_path, "");
    /// ```
    pub fn validate(&self, value: Value) -> Result<Value, SchemaViolations> {
        let violations = match self.check(&value) {
            Ok(()) => return Ok(value),
            Err(v) => SchemaViolations(v),
        };

        let Value::String(s) = &value else {
            return Err(violations);
        };
        let Ok(decoded) = serde_json::from_str::<Value>(s) else {
            return Err(violations);
        };

        if self.check(&decoded).is_ok() {
            Ok(decoded)
        } else {
            Err(violations)
        }
    }

    /// Check `value` against the schema and report every violation, each
    /// naming where in the value it occurred.
    fn check(&self, instance: &Value) -> Result<(), Vec<SchemaViolation>> {
        let mut violations = Vec::new();
        self.inner.compiled.check(instance, "", &mut violations);
        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }
}

impl fmt::Debug for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Schema").finish_non_exhaustive()
    }
}

impl serde::Serialize for Schema {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.raw_document.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Schema {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = Value::deserialize(deserializer)?;
        Schema::new(document).map_err(serde::de::Error::custom)
    }
}

/// One thing wrong with a value, reported by [`Schema::validate`].
#[derive(Debug, Clone)]
pub struct SchemaViolation {
    /// Where in the value the problem is, as a JSON Pointer such as
    /// `/items/0/name`. Empty means the value itself.
    pub instance_path: String,
    /// Which part of the schema was violated, as a JSON Pointer.
    pub schema_path: String,
    /// What went wrong, in one line.
    pub message: String,
}

impl fmt::Display for SchemaViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A root violation has no pointer to name, so show the message alone
        // rather than a placeholder path.
        if self.instance_path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.instance_path, self.message)
        }
    }
}

/// Everything wrong with one value. Its `Display` is what the agent reads back:
/// a header and one line per violation.
#[derive(Debug, Clone)]
pub struct SchemaViolations(Vec<SchemaViolation>);

impl std::ops::Deref for SchemaViolations {
    type Target = [SchemaViolation];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for SchemaViolations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Schema validation failed:")?;
        for violation in &self.0 {
            write!(f, "\n- {violation}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SchemaViolations {}

/// The schema itself is invalid. A [`SchemaViolation`] is the other case: a
/// value failing a schema that is fine.
#[derive(Debug, Clone)]
pub struct SchemaParseError {
    pub message: String,
}

impl fmt::Display for SchemaParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid schema: {}", self.message)
    }
}

impl std::error::Error for SchemaParseError {}

// Compiled schema tree

#[derive(Debug, Default)]
struct Node {
    schema_path: String,
    // type / enum / const
    types: Option<Vec<JsonType>>,
    enum_values: Option<Vec<Value>>,
    const_value: Option<Value>,
    // logical combinators
    all_of: Option<Vec<Node>>,
    any_of: Option<Vec<Node>>,
    one_of: Option<Vec<Node>>,
    not: Option<Box<Node>>,
    // conditional
    if_schema: Option<Box<Node>>,
    then_schema: Option<Box<Node>>,
    else_schema: Option<Box<Node>>,
    // object
    properties: Option<Vec<(String, Node)>>,
    required: Option<Vec<String>>,
    additional_properties: AdditionalProperties,
    // array
    items: Option<Box<Node>>,
    min_items: Option<usize>,
    max_items: Option<usize>,
    // number
    minimum: Option<f64>,
    maximum: Option<f64>,
    // string
    min_length: Option<usize>,
    max_length: Option<usize>,
    pattern: Option<regex::Regex>,
}

#[derive(Debug, Default)]
enum AdditionalProperties {
    /// `additionalProperties` unset or `true`: extra keys are allowed.
    #[default]
    Allowed,
    /// `additionalProperties: false`: an extra key is a violation.
    Forbidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonType {
    Object,
    Array,
    String,
    Number,
    Integer,
    Boolean,
    Null,
}

impl JsonType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "object" => Self::Object,
            "array" => Self::Array,
            "string" => Self::String,
            "number" => Self::Number,
            "integer" => Self::Integer,
            "boolean" => Self::Boolean,
            "null" => Self::Null,
            _ => return None,
        })
    }

    fn matches(self, value: &Value) -> bool {
        match (self, value) {
            (Self::Object, Value::Object(_)) => true,
            (Self::Array, Value::Array(_)) => true,
            (Self::String, Value::String(_)) => true,
            (Self::Boolean, Value::Bool(_)) => true,
            (Self::Null, Value::Null) => true,
            (Self::Number, Value::Number(_)) => true,
            (Self::Integer, Value::Number(n)) => is_integer(n),
            _ => false,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Array => "array",
            Self::String => "string",
            Self::Number => "number",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::Null => "null",
        }
    }
}

fn is_integer(n: &Number) -> bool {
    if n.is_i64() || n.is_u64() {
        return true;
    }
    // f64 with no fractional part is still considered an integer by
    // JSON Schema (e.g. `1.0` validates against `type: integer`).
    n.as_f64()
        .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
}

/// The keywords agentwerk understands. Anything else is rejected when the
/// schema is parsed, so an unsupported constraint is reported rather than
/// quietly ignored.
const SUPPORTED_KEYWORDS: &[&str] = &[
    // assertions
    "type",
    "required",
    "properties",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
    // applicators
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "if",
    "then",
    "else",
    // informational: accepted but not evaluated
    "$schema",
    "$id",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
];

fn compile(value: &Value, schema_path: &str) -> Result<Node, SchemaParseError> {
    let obj = match value {
        Value::Object(map) => map,
        Value::Bool(true) => {
            return Ok(Node {
                schema_path: schema_path.to_string(),
                ..Node::default()
            });
        }
        Value::Bool(false) => {
            // false schema rejects everything; model as empty types list.
            return Ok(Node {
                schema_path: schema_path.to_string(),
                types: Some(Vec::new()),
                ..Node::default()
            });
        }
        other => {
            return Err(parse_err(
                schema_path,
                format!("expected an object or boolean, got {}", value_label(other)),
            ));
        }
    };

    for key in obj.keys() {
        if !SUPPORTED_KEYWORDS.contains(&key.as_str()) {
            return Err(parse_err(
                schema_path,
                format!("unsupported keyword `{key}`"),
            ));
        }
    }

    let types = match obj.get("type") {
        None => None,
        Some(Value::String(s)) => Some(vec![parse_type(s, schema_path, "type")?]),
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let s = item.as_str().ok_or_else(|| {
                    parse_err(
                        schema_path,
                        format!("`type[{i}]` must be a string, got {}", value_label(item)),
                    )
                })?;
                out.push(parse_type(s, schema_path, &format!("type[{i}]"))?);
            }
            Some(out)
        }
        Some(other) => {
            return Err(parse_err(
                schema_path,
                format!(
                    "`type` must be a string or array of strings, got {}",
                    value_label(other)
                ),
            ));
        }
    };

    let enum_values = match obj.get("enum") {
        None => None,
        Some(Value::Array(arr)) if !arr.is_empty() => Some(arr.clone()),
        Some(Value::Array(_)) => {
            return Err(parse_err(
                schema_path,
                "`enum` must contain at least one value",
            ));
        }
        Some(other) => {
            return Err(parse_err(
                schema_path,
                format!(
                    "`enum` must be a non-empty array, got {}",
                    value_label(other)
                ),
            ));
        }
    };

    let const_value = obj.get("const").cloned();

    let required = match obj.get("required") {
        None => None,
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let s = item.as_str().ok_or_else(|| {
                    parse_err(
                        schema_path,
                        format!(
                            "`required[{i}]` must be a string, got {}",
                            value_label(item)
                        ),
                    )
                })?;
                out.push(s.to_string());
            }
            Some(out)
        }
        Some(other) => {
            return Err(parse_err(
                schema_path,
                format!(
                    "`required` must be an array of strings, got {}",
                    value_label(other)
                ),
            ));
        }
    };

    let properties = match obj.get("properties") {
        None => None,
        Some(Value::Object(props)) => {
            let mut out = Vec::with_capacity(props.len());
            for (name, sub) in props {
                let sub_path = format!("{schema_path}/properties/{}", escape_pointer(name));
                let node = compile(sub, &sub_path)?;
                out.push((name.clone(), node));
            }
            Some(out)
        }
        Some(other) => {
            return Err(parse_err(
                schema_path,
                format!("`properties` must be an object, got {}", value_label(other)),
            ));
        }
    };

    let additional_properties = match obj.get("additionalProperties") {
        None => AdditionalProperties::Allowed,
        Some(Value::Bool(true)) => AdditionalProperties::Allowed,
        Some(Value::Bool(false)) => AdditionalProperties::Forbidden,
        Some(other) => {
            return Err(parse_err(
                schema_path,
                format!(
                    "`additionalProperties` must be a boolean (subschema form is unsupported), got {}",
                    value_label(other)
                ),
            ));
        }
    };

    let items = match obj.get("items") {
        None => None,
        Some(v @ (Value::Object(_) | Value::Bool(_))) => {
            let sub_path = format!("{schema_path}/items");
            Some(Box::new(compile(v, &sub_path)?))
        }
        Some(other) => {
            return Err(parse_err(
                schema_path,
                format!(
                    "`items` must be a schema (object or boolean); per-position arrays are unsupported, got {}",
                    value_label(other)
                ),
            ));
        }
    };

    let all_of = parse_subschema_array(obj, "allOf", schema_path)?;
    let any_of = parse_subschema_array(obj, "anyOf", schema_path)?;
    let one_of = parse_subschema_array(obj, "oneOf", schema_path)?;
    let not = parse_subschema(obj, "not", schema_path)?;

    let if_schema = parse_subschema(obj, "if", schema_path)?;
    let then_schema = parse_subschema(obj, "then", schema_path)?;
    let else_schema = parse_subschema(obj, "else", schema_path)?;

    let minimum = parse_number(obj.get("minimum"), schema_path, "minimum")?;
    let maximum = parse_number(obj.get("maximum"), schema_path, "maximum")?;
    let min_length = parse_usize(obj.get("minLength"), schema_path, "minLength")?;
    let max_length = parse_usize(obj.get("maxLength"), schema_path, "maxLength")?;
    let min_items = parse_usize(obj.get("minItems"), schema_path, "minItems")?;
    let max_items = parse_usize(obj.get("maxItems"), schema_path, "maxItems")?;

    let pattern = match obj.get("pattern") {
        None => None,
        Some(Value::String(s)) => Some(compile_regex(s, schema_path, "pattern")?),
        Some(other) => {
            return Err(parse_err(
                schema_path,
                format!("`pattern` must be a string, got {}", value_label(other)),
            ));
        }
    };

    Ok(Node {
        schema_path: schema_path.to_string(),
        types,
        enum_values,
        const_value,
        all_of,
        any_of,
        one_of,
        not,
        if_schema,
        then_schema,
        else_schema,
        properties,
        required,
        additional_properties,
        items,
        min_items,
        max_items,
        minimum,
        maximum,
        min_length,
        max_length,
        pattern,
    })
}

/// Read one nested schema, written either as an object or as a boolean.
fn parse_subschema(
    obj: &Map<String, Value>,
    key: &str,
    schema_path: &str,
) -> Result<Option<Box<Node>>, SchemaParseError> {
    match obj.get(key) {
        None => Ok(None),
        Some(v @ (Value::Object(_) | Value::Bool(_))) => {
            Ok(Some(Box::new(compile(v, &format!("{schema_path}/{key}"))?)))
        }
        Some(other) => Err(parse_err(
            schema_path,
            format!(
                "`{key}` must be a schema (object or boolean), got {}",
                value_label(other)
            ),
        )),
    }
}

/// Read the nested schemas of `allOf`, `anyOf`, or `oneOf`, of which there must
/// be at least one.
fn parse_subschema_array(
    obj: &Map<String, Value>,
    key: &str,
    schema_path: &str,
) -> Result<Option<Vec<Node>>, SchemaParseError> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Array(arr)) if !arr.is_empty() => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let sub_path = format!("{schema_path}/{key}/{i}");
                out.push(compile(item, &sub_path)?);
            }
            Ok(Some(out))
        }
        Some(Value::Array(_)) => Err(parse_err(
            schema_path,
            format!("`{key}` must contain at least one schema"),
        )),
        Some(other) => Err(parse_err(
            schema_path,
            format!(
                "`{key}` must be a non-empty array of schemas, got {}",
                value_label(other)
            ),
        )),
    }
}

fn parse_type(s: &str, schema_path: &str, key: &str) -> Result<JsonType, SchemaParseError> {
    JsonType::parse(s)
        .ok_or_else(|| parse_err(schema_path, format!("`{key}` has unknown type `{s}`")))
}

fn parse_number(
    v: Option<&Value>,
    schema_path: &str,
    key: &str,
) -> Result<Option<f64>, SchemaParseError> {
    match v {
        None => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_f64()),
        Some(other) => Err(parse_err(
            schema_path,
            format!("`{key}` must be a number, got {}", value_label(other)),
        )),
    }
}

fn parse_usize(
    v: Option<&Value>,
    schema_path: &str,
    key: &str,
) -> Result<Option<usize>, SchemaParseError> {
    match v {
        None => Ok(None),
        Some(Value::Number(n)) => {
            let u = n.as_u64().ok_or_else(|| {
                parse_err(
                    schema_path,
                    format!("`{key}` must be a non-negative integer, got {n}"),
                )
            })?;
            Ok(Some(u as usize))
        }
        Some(other) => Err(parse_err(
            schema_path,
            format!(
                "`{key}` must be a non-negative integer, got {}",
                value_label(other)
            ),
        )),
    }
}

fn compile_regex(
    pattern: &str,
    schema_path: &str,
    key: &str,
) -> Result<regex::Regex, SchemaParseError> {
    regex::Regex::new(pattern)
        .map_err(|e| parse_err(schema_path, format!("`{key}` is not a valid regex: {e}")))
}

fn parse_err(schema_path: &str, message: impl Into<String>) -> SchemaParseError {
    let prefix = if schema_path.is_empty() {
        "<root>".to_string()
    } else {
        schema_path.to_string()
    };
    SchemaParseError {
        message: format!("at {prefix}: {}", message.into()),
    }
}

fn value_label(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn escape_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

// Validation

impl Node {
    fn check(&self, instance: &Value, instance_path: &str, out: &mut Vec<SchemaViolation>) {
        if let Some(types) = &self.types {
            if types.is_empty() {
                self.violation(
                    instance_path,
                    "/type",
                    "value is rejected by `false` schema",
                    out,
                );
                return;
            }
            if !types.iter().any(|t| t.matches(instance)) {
                let labels: Vec<&str> = types.iter().map(|t| t.label()).collect();
                self.violation(
                    instance_path,
                    "/type",
                    format!(
                        "expected type {}, got {}",
                        join_or(&labels),
                        value_label(instance)
                    ),
                    out,
                );
                return;
            }
        }

        if let Some(expected) = &self.const_value {
            if instance != expected {
                self.violation(
                    instance_path,
                    "/const",
                    format!("expected {}", expected),
                    out,
                );
            }
        }

        if let Some(values) = &self.enum_values {
            if !values.iter().any(|v| v == instance) {
                self.violation(instance_path, "/enum", "value is not in `enum`", out);
            }
        }

        // allOf collects violations from every branch, not just the first failure
        if let Some(schemas) = &self.all_of {
            for sub in schemas {
                sub.check(instance, instance_path, out);
            }
        }

        if let Some(schemas) = &self.any_of {
            let passed = schemas.iter().any(|sub| {
                let mut tmp = Vec::new();
                sub.check(instance, instance_path, &mut tmp);
                tmp.is_empty()
            });
            if !passed {
                self.violation(
                    instance_path,
                    "/anyOf",
                    "value does not match any of the anyOf schemas",
                    out,
                );
            }
        }

        if let Some(schemas) = &self.one_of {
            let count = schemas
                .iter()
                .filter(|sub| {
                    let mut tmp = Vec::new();
                    sub.check(instance, instance_path, &mut tmp);
                    tmp.is_empty()
                })
                .count();
            if count != 1 {
                self.violation(
                    instance_path,
                    "/oneOf",
                    format!("value matches {count} of the oneOf schemas, expected exactly 1"),
                    out,
                );
            }
        }

        if let Some(sub) = &self.not {
            let mut tmp = Vec::new();
            sub.check(instance, instance_path, &mut tmp);
            if tmp.is_empty() {
                self.violation(
                    instance_path,
                    "/not",
                    "value must not match the `not` schema",
                    out,
                );
            }
        }

        if let Some(if_sub) = &self.if_schema {
            let mut tmp = Vec::new();
            if_sub.check(instance, instance_path, &mut tmp);
            if tmp.is_empty() {
                if let Some(then_sub) = &self.then_schema {
                    then_sub.check(instance, instance_path, out);
                }
            } else if let Some(else_sub) = &self.else_schema {
                else_sub.check(instance, instance_path, out);
            }
        }

        match instance {
            Value::Object(map) => self.check_object(map, instance_path, out),
            Value::Array(arr) => self.check_array(arr, instance_path, out),
            Value::String(s) => self.check_string(s, instance_path, out),
            Value::Number(n) => self.check_number(n, instance_path, out),
            _ => {}
        }
    }

    fn check_object(
        &self,
        map: &Map<String, Value>,
        instance_path: &str,
        out: &mut Vec<SchemaViolation>,
    ) {
        if let Some(req) = &self.required {
            for name in req {
                if !map.contains_key(name) {
                    self.violation(
                        instance_path,
                        "/required",
                        format!("missing required property `{name}`"),
                        out,
                    );
                }
            }
        }

        if let Some(props) = &self.properties {
            for (name, sub) in props {
                if let Some(v) = map.get(name) {
                    let child_path = format!("{instance_path}/{}", escape_pointer(name));
                    sub.check(v, &child_path, out);
                }
            }
        }

        if matches!(self.additional_properties, AdditionalProperties::Forbidden) {
            let known: HashSet<&str> = self
                .properties
                .as_ref()
                .map(|ps| ps.iter().map(|(k, _)| k.as_str()).collect())
                .unwrap_or_default();
            for name in map.keys() {
                if !known.contains(name.as_str()) {
                    self.violation(
                        instance_path,
                        "/additionalProperties",
                        format!("unexpected property `{name}`"),
                        out,
                    );
                }
            }
        }
    }

    fn check_array(&self, arr: &[Value], instance_path: &str, out: &mut Vec<SchemaViolation>) {
        if let Some(min) = self.min_items {
            if arr.len() < min {
                self.violation(
                    instance_path,
                    "/minItems",
                    format!("array has {} items, expected at least {min}", arr.len()),
                    out,
                );
            }
        }
        if let Some(max) = self.max_items {
            if arr.len() > max {
                self.violation(
                    instance_path,
                    "/maxItems",
                    format!("array has {} items, expected at most {max}", arr.len()),
                    out,
                );
            }
        }
        if let Some(items_schema) = &self.items {
            for (i, item) in arr.iter().enumerate() {
                let child_path = format!("{instance_path}/{i}");
                items_schema.check(item, &child_path, out);
            }
        }
    }

    fn check_string(&self, s: &str, instance_path: &str, out: &mut Vec<SchemaViolation>) {
        let len = s.chars().count();
        if let Some(min) = self.min_length {
            if len < min {
                self.violation(
                    instance_path,
                    "/minLength",
                    format!("string length {len} is below minimum {min}"),
                    out,
                );
            }
        }
        if let Some(max) = self.max_length {
            if len > max {
                self.violation(
                    instance_path,
                    "/maxLength",
                    format!("string length {len} is above maximum {max}"),
                    out,
                );
            }
        }
        if let Some(re) = &self.pattern {
            if !re.is_match(s) {
                self.violation(
                    instance_path,
                    "/pattern",
                    format!("string does not match pattern `{}`", re.as_str()),
                    out,
                );
            }
        }
    }

    fn check_number(&self, n: &Number, instance_path: &str, out: &mut Vec<SchemaViolation>) {
        let Some(f) = n.as_f64() else { return };
        if let Some(min) = self.minimum {
            if f < min {
                self.violation(
                    instance_path,
                    "/minimum",
                    format!("value {f} is below minimum {min}"),
                    out,
                );
            }
        }
        if let Some(max) = self.maximum {
            if f > max {
                self.violation(
                    instance_path,
                    "/maximum",
                    format!("value {f} is above maximum {max}"),
                    out,
                );
            }
        }
    }

    fn violation(
        &self,
        instance_path: &str,
        keyword_suffix: &str,
        message: impl Into<String>,
        out: &mut Vec<SchemaViolation>,
    ) {
        out.push(SchemaViolation {
            instance_path: instance_path.to_string(),
            schema_path: format!("{}{keyword_suffix}", self.schema_path),
            message: message.into(),
        });
    }
}

fn join_or(labels: &[&str]) -> String {
    match labels.len() {
        0 => String::new(),
        1 => labels[0].to_string(),
        _ => {
            let (last, head) = labels.split_last().unwrap();
            format!("{} or {}", head.join(", "), last)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Parse errors

    #[test]
    fn parse_rejects_malformed_schema() {
        let bad = json!({"type": 42});
        let err = Schema::new(bad).unwrap_err();
        assert!(err.message.contains("`type` must be"));
    }

    #[test]
    fn parse_rejects_unknown_type_label() {
        let err = Schema::new(json!({"type": "thingy"})).unwrap_err();
        assert!(err.message.contains("unknown type"));
    }

    #[test]
    fn parse_rejects_unsupported_keyword() {
        // `uniqueItems` is outside the supported subset; the limit must
        // surface at parse time rather than silently passing values it
        // would otherwise constrain.
        let err = Schema::new(json!({"type": "array", "uniqueItems": true})).unwrap_err();
        assert!(err.message.contains("unsupported keyword `uniqueItems`"));
    }

    #[test]
    fn parse_rejects_additional_properties_subschema_form() {
        let err = Schema::new(json!({"additionalProperties": {"type": "integer"}})).unwrap_err();
        assert!(err.message.contains("subschema form is unsupported"));
    }

    #[test]
    fn parse_rejects_invalid_regex() {
        let err = Schema::new(json!({"pattern": "["})).unwrap_err();
        assert!(err.message.contains("pattern"));
    }

    #[test]
    fn parse_rejects_empty_all_of() {
        let err = Schema::new(json!({"allOf": []})).unwrap_err();
        assert!(err.message.contains("allOf"));
    }

    #[test]
    fn parse_rejects_empty_any_of() {
        let err = Schema::new(json!({"anyOf": []})).unwrap_err();
        assert!(err.message.contains("anyOf"));
    }

    #[test]
    fn parse_rejects_empty_one_of() {
        let err = Schema::new(json!({"oneOf": []})).unwrap_err();
        assert!(err.message.contains("oneOf"));
    }

    // Type

    #[test]
    fn validate_type_rejects_wrong_kind() {
        let schema = Schema::new(json!({"type": "object", "required": ["status"]})).unwrap();
        assert!(schema.validate(json!({"status": "ok"})).is_ok());
        assert!(schema.validate(json!(42)).is_err());
    }

    #[test]
    fn validate_type_array_accepts_any_listed() {
        let schema = Schema::new(json!({"type": ["string", "null"]})).unwrap();
        assert!(schema.validate(json!("hi")).is_ok());
        assert!(schema.validate(json!(null)).is_ok());
        assert!(schema.validate(json!(1)).is_err());
    }

    #[test]
    fn validate_integer_accepts_whole_floats() {
        let schema = Schema::new(json!({"type": "integer"})).unwrap();
        assert!(schema.validate(json!(1.0)).is_ok());
        assert!(schema.validate(json!(1.5)).is_err());
    }

    #[test]
    fn boolean_true_schema_accepts_anything() {
        let schema = Schema::new(json!(true)).unwrap();
        assert!(schema.validate(json!(null)).is_ok());
        assert!(schema.validate(json!({"a": [1, 2]})).is_ok());
    }

    #[test]
    fn boolean_false_schema_rejects_everything() {
        let schema = Schema::new(json!(false)).unwrap();
        assert!(schema.validate(json!(null)).is_err());
        assert!(schema.validate(json!("anything")).is_err());
    }

    // Enum / const

    #[test]
    fn validate_enum_rejects_value_not_in_list() {
        let schema = Schema::new(json!({"enum": ["a", "b", "c"]})).unwrap();
        assert!(schema.validate(json!("b")).is_ok());
        assert!(schema.validate(json!("z")).is_err());
    }

    #[test]
    fn validate_const_rejects_non_matching_value() {
        let schema = Schema::new(json!({"const": 42})).unwrap();
        assert!(schema.validate(json!(42)).is_ok());
        assert!(schema.validate(json!(43)).is_err());
    }

    // Object

    #[test]
    fn validate_passes_conforming_object() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"],
        }))
        .unwrap();
        assert!(schema.validate(json!({"name": "alice"})).is_ok());
    }

    #[test]
    fn validate_reports_each_violation_with_path() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer", "minimum": 0 },
            },
            "required": ["name", "age"],
        }))
        .unwrap();
        let violations = schema.validate(json!({"name": 7, "age": -1})).unwrap_err();
        assert!(violations.len() >= 2);
        let paths: Vec<&str> = violations
            .iter()
            .map(|v| v.instance_path.as_str())
            .collect();
        assert!(paths.iter().any(|p| p.contains("/name")));
        assert!(paths.iter().any(|p| p.contains("/age")));
    }

    #[test]
    fn validate_reports_missing_required() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "x": { "type": "string" } },
            "required": ["x", "y"],
        }))
        .unwrap();
        let violations = schema.validate(json!({"x": "hi"})).unwrap_err();
        assert!(violations.iter().any(|v| v.message.contains("`y`")));
    }

    #[test]
    fn validate_additional_properties_forbidden() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "x": { "type": "string" } },
            "additionalProperties": false,
        }))
        .unwrap();
        assert!(schema.validate(json!({"x": "hi"})).is_ok());
        let violations = schema.validate(json!({"x": "hi", "y": 1})).unwrap_err();
        assert!(violations.iter().any(|v| v.message.contains("`y`")));
    }

    // Array

    #[test]
    fn validate_items_schema_validates_each_element() {
        let schema =
            Schema::new(json!({"type": "array", "items": {"type": "integer", "minimum": 0}}))
                .unwrap();
        assert!(schema.validate(json!([1, 2, 3])).is_ok());
        let violations = schema.validate(json!([0, -1])).unwrap_err();
        assert!(violations.iter().any(|v| v.instance_path == "/1"));
    }

    #[test]
    fn validate_min_items_requires_minimum_count() {
        let schema = Schema::new(json!({"type": "array", "minItems": 1})).unwrap();
        assert!(schema.validate(json!([1])).is_ok());
        let violations = schema.validate(json!([])).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| v.schema_path.ends_with("/minItems")));
    }

    #[test]
    fn validate_max_items_rejects_excess_count() {
        let schema = Schema::new(json!({"type": "array", "maxItems": 2})).unwrap();
        assert!(schema.validate(json!([1, 2])).is_ok());
        let violations = schema.validate(json!([1, 2, 3])).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| v.schema_path.ends_with("/maxItems")));
    }

    // String

    #[test]
    fn validate_string_length_bounds() {
        let schema =
            Schema::new(json!({"type": "string", "minLength": 2, "maxLength": 4})).unwrap();
        assert!(schema.validate(json!("ok")).is_ok());
        assert!(schema.validate(json!("a")).is_err());
        assert!(schema.validate(json!("toolong")).is_err());
    }

    #[test]
    fn validate_pattern_matches_string() {
        let schema = Schema::new(json!({"pattern": "^foo"})).unwrap();
        assert!(schema.validate(json!("foobar")).is_ok());
        let violations = schema.validate(json!("bar")).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| v.schema_path.ends_with("/pattern")));
    }

    #[test]
    fn validate_pattern_is_unanchored() {
        let schema = Schema::new(json!({"pattern": "foo"})).unwrap();
        assert!(schema.validate(json!("barfoobar")).is_ok());
    }

    #[test]
    fn validate_pattern_ignored_for_non_strings() {
        let schema = Schema::new(json!({"pattern": "foo"})).unwrap();
        assert!(schema.validate(json!(42)).is_ok());
    }

    // Number

    #[test]
    fn validate_minimum_and_maximum_bounds() {
        let schema = Schema::new(json!({"minimum": 0, "maximum": 10})).unwrap();
        assert!(schema.validate(json!(5)).is_ok());
        assert!(schema.validate(json!(-1)).is_err());
        assert!(schema.validate(json!(11)).is_err());
    }

    // Logical combinators

    #[test]
    fn validate_all_of_passes_when_all_schemas_match() {
        let schema = Schema::new(json!({
            "allOf": [
                { "type": "object" },
                { "required": ["name"] },
            ]
        }))
        .unwrap();
        assert!(schema.validate(json!({"name": "alice"})).is_ok());
        assert!(schema.validate(json!({})).is_err());
    }

    #[test]
    fn validate_all_of_collects_violations_from_all_failing_schemas() {
        let schema = Schema::new(json!({
            "allOf": [
                { "required": ["a"] },
                { "required": ["b"] },
            ]
        }))
        .unwrap();
        let violations = schema.validate(json!({})).unwrap_err();
        assert!(violations.iter().any(|v| v.message.contains("`a`")));
        assert!(violations.iter().any(|v| v.message.contains("`b`")));
    }

    #[test]
    fn validate_any_of_passes_when_at_least_one_matches() {
        let schema = Schema::new(json!({
            "anyOf": [
                { "type": "string" },
                { "type": "number" },
            ]
        }))
        .unwrap();
        assert!(schema.validate(json!("hello")).is_ok());
        assert!(schema.validate(json!(42)).is_ok());
        let violations = schema.validate(json!(null)).unwrap_err();
        assert!(violations.iter().any(|v| v.schema_path.ends_with("/anyOf")));
    }

    #[test]
    fn validate_one_of_requires_exactly_one_match() {
        let schema = Schema::new(json!({
            "oneOf": [
                { "type": "string" },
                { "type": "number" },
            ]
        }))
        .unwrap();
        // Matches exactly one branch.
        assert!(schema.validate(json!("hello")).is_ok());
        assert!(schema.validate(json!(42)).is_ok());
        // Matches neither branch.
        let none = schema.validate(json!(true)).unwrap_err();
        assert!(none.iter().any(|v| v.schema_path.ends_with("/oneOf")));

        // A number matches both `number` and `minimum`, so oneOf fails.
        let both = Schema::new(json!({
            "oneOf": [
                { "type": "number" },
                { "minimum": 0 },
            ]
        }))
        .unwrap();
        let violations = both.validate(json!(5)).unwrap_err();
        assert!(violations.iter().any(|v| v.schema_path.ends_with("/oneOf")));
    }

    #[test]
    fn validate_not_inverts_subschema() {
        let schema = Schema::new(json!({"not": {"type": "string"}})).unwrap();
        assert!(schema.validate(json!(42)).is_ok());
        let violations = schema.validate(json!("hello")).unwrap_err();
        assert!(violations.iter().any(|v| v.schema_path.ends_with("/not")));
    }

    // If / then / else

    #[test]
    fn validate_if_then_applies_then_when_if_passes() {
        let schema = Schema::new(json!({
            "if":   { "type": "string" },
            "then": { "minLength": 3 },
        }))
        .unwrap();
        assert!(schema.validate(json!("hello")).is_ok());
        assert!(schema.validate(json!("hi")).is_err());
        assert!(schema.validate(json!(42)).is_ok());
    }

    #[test]
    fn validate_if_then_else_selects_correct_branch() {
        let schema = Schema::new(json!({
            "if":   { "type": "string" },
            "then": { "minLength": 3 },
            "else": { "minimum": 0 },
        }))
        .unwrap();
        assert!(schema.validate(json!("hello")).is_ok());
        assert!(schema.validate(json!("hi")).is_err()); // then fires, fails
        assert!(schema.validate(json!(5)).is_ok());
        assert!(schema.validate(json!(-1)).is_err()); // else fires, fails
    }

    #[test]
    fn validate_if_without_then_else_has_no_effect() {
        let schema = Schema::new(json!({"if": {"type": "string"}})).unwrap();
        assert!(schema.validate(json!("hello")).is_ok());
        assert!(schema.validate(json!(42)).is_ok());
    }

    // Decode fallback

    #[test]
    fn validate_returns_a_conforming_value_unchanged() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "status": { "type": "string" } },
            "required": ["status"],
        }))
        .unwrap();
        let value = json!({"status": "ok"});
        assert_eq!(schema.validate(value.clone()).unwrap(), value);
    }

    #[test]
    fn validate_decodes_a_string_encoded_object() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "status": { "type": "string" } },
            "required": ["status"],
        }))
        .unwrap();
        let encoded = json!("{\"status\": \"ok\"}");
        assert_eq!(schema.validate(encoded).unwrap(), json!({"status": "ok"}));
    }

    #[test]
    fn validate_reports_string_type_violation_when_decoded_object_also_fails() {
        // The string decodes to a valid object, but that object still fails
        // `required`. The violation reported must be the outer /type error,
        // not the inner /required, so the agent retry targets the real problem.
        let schema = Schema::new(json!({"type": "object", "required": ["status"]})).unwrap();
        let violations = schema.validate(json!("{\"other\": 1}")).unwrap_err();
        assert!(violations.iter().any(|v| v.schema_path.ends_with("/type")));
        assert!(!violations
            .iter()
            .any(|v| v.schema_path.ends_with("/required")));
    }

    #[test]
    fn validate_non_string_fails_type_check_without_decode_attempt() {
        let schema = Schema::new(json!({"type": "object", "required": ["status"]})).unwrap();
        assert!(schema.validate(json!(42)).is_err());
    }

    // Serde / clone

    #[test]
    fn clone_shares_compiled_state() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "x": { "type": "string" } },
            "required": ["x"],
        }))
        .unwrap();
        let cloned = schema.clone();
        assert!(schema.validate(json!({"x": "hi"})).is_ok());
        assert!(cloned.validate(json!({"x": "hi"})).is_ok());
        assert!(schema.validate(json!({})).is_err());
        assert!(cloned.validate(json!({})).is_err());
        // The underlying compiled node is the same allocation (Arc-backed).
        assert!(Arc::ptr_eq(&schema.inner, &cloned.inner));
    }

    #[test]
    fn schema_parse_round_trips_through_serde() {
        let document = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"],
        });
        let schema = Schema::new(document.clone()).unwrap();
        let serialised = serde_json::to_value(&schema).unwrap();
        assert_eq!(serialised, document);
        let restored: Schema = serde_json::from_value(serialised).unwrap();
        assert!(restored.validate(json!({"name": "alice"})).is_ok());
        assert!(restored.validate(json!({"age": 7})).is_err());
    }

    #[test]
    fn option_schema_deserializes_null_as_none() {
        let none: Option<Schema> = serde_json::from_value(Value::Null).unwrap();
        assert!(none.is_none());
    }

    // SchemaViolations display (agent-facing feedback)

    #[test]
    fn violations_display_renders_one_line_per_violation() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "x": { "type": "string" } },
            "required": ["x", "y"],
        }))
        .unwrap();
        let violations = schema.validate(json!({"x": 1})).unwrap_err();
        let formatted = violations.to_string();
        assert!(formatted.starts_with("Schema validation failed:\n"));
        let body = formatted.trim_start_matches("Schema validation failed:\n");
        assert!(body.lines().all(|line| line.starts_with("- ")));
    }
}
