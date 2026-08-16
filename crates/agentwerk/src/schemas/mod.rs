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
//! // A conforming value comes back unchanged, repaired nowhere.
//! let value = json!({ "name": "Ada" });
//! assert_eq!(schema.validate(value.clone()).unwrap(), (value, vec![]));
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

    /// Validate content and give back the value to keep, plus the JSON pointer
    /// of every value it repaired to get there, empty for a repair of the value
    /// as a whole.
    ///
    /// A value that satisfies the schema comes back unchanged and repaired
    /// nowhere. A value that does not is retyped against the schema and then
    /// checked: a scalar an agent quoted becomes the type declared for it, and
    /// a structure it wrote as JSON text is decoded. The violations describe
    /// what the retype produced, so one report names everything still wrong
    /// instead of one problem per attempt.
    ///
    /// ```
    /// use agentwerk::schemas::Schema;
    /// use serde_json::json;
    ///
    /// let schema = Schema::new(json!({
    ///     "type": "object",
    ///     "properties": { "line": { "type": "integer" } },
    ///     "required": ["line"],
    /// }))
    /// .unwrap();
    ///
    /// // A string an agent double-encoded is decoded, then validated.
    /// let (decoded, _) = schema.validate(json!("{\"line\": 12}")).unwrap();
    /// assert_eq!(decoded, json!({ "line": 12 }));
    ///
    /// // A number an agent quoted is retyped to what the schema declares, and
    /// // the repair names where it happened.
    /// let (retyped, repaired) = schema.validate(json!({ "line": "12" })).unwrap();
    /// assert_eq!(retyped, json!({ "line": 12 }));
    /// assert_eq!(repaired, vec!["/line"]);
    ///
    /// // A failure carries the instance path of each problem.
    /// let violations = schema.validate(json!({})).unwrap_err();
    /// assert_eq!(violations[0].instance_path, "");
    /// ```
    pub fn validate(&self, value: Value) -> Result<(Value, Vec<String>), SchemaViolations> {
        // Checking first is both the fast path and the guarantee that a
        // conforming value comes back untouched.
        if self.check(&value).is_ok() {
            return Ok((value, Vec::new()));
        }
        let mut repaired = value;
        let mut repairs = Vec::new();
        self.inner.compiled.coerce(&mut repaired, "", &mut repairs);
        match self.check(&repaired) {
            Ok(()) => Ok((repaired, repairs)),
            Err(violations) => Err(SchemaViolations(violations)),
        }
    }

    /// The JSON Schema document this was built from, the same one `Serialize`
    /// writes. Reading a field off it needs no copy and cannot fail.
    pub(crate) fn get_raw_schema(&self) -> &Value {
        &self.inner.raw_document
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
    additional_properties_forbidden: bool,
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
            // JSON Schema counts a whole float as an integer: `1.0` validates
            // against `type: integer`.
            (Self::Integer, Value::Number(n)) => {
                n.is_i64()
                    || n.is_u64()
                    || n.as_f64()
                        .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
            }
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

// Parsing

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
            // A false schema rejects everything: no type matches an empty list.
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
                let key = format!("type[{i}]");
                let s = item
                    .as_str()
                    .ok_or_else(|| wrong_type(schema_path, &key, "a string", item))?;
                out.push(parse_type(s, schema_path, &key)?);
            }
            Some(out)
        }
        Some(other) => {
            return Err(wrong_type(
                schema_path,
                "type",
                "a string or array of strings",
                other,
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
            return Err(wrong_type(schema_path, "enum", "a non-empty array", other));
        }
    };

    let const_value = obj.get("const").cloned();

    let required = match obj.get("required") {
        None => None,
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let s = item.as_str().ok_or_else(|| {
                    wrong_type(schema_path, &format!("required[{i}]"), "a string", item)
                })?;
                out.push(s.to_string());
            }
            Some(out)
        }
        Some(other) => {
            return Err(wrong_type(
                schema_path,
                "required",
                "an array of strings",
                other,
            ));
        }
    };

    let properties = match obj.get("properties") {
        None => None,
        Some(Value::Object(props)) => {
            let mut out = Vec::with_capacity(props.len());
            for (name, sub) in props {
                let sub_path = format!("{schema_path}/properties/{}", escape_pointer(name));
                out.push((name.clone(), compile(sub, &sub_path)?));
            }
            Some(out)
        }
        Some(other) => {
            return Err(wrong_type(schema_path, "properties", "an object", other));
        }
    };

    let additional_properties_forbidden = match obj.get("additionalProperties") {
        None | Some(Value::Bool(true)) => false,
        Some(Value::Bool(false)) => true,
        Some(other) => {
            return Err(wrong_type(
                schema_path,
                "additionalProperties",
                "a boolean (subschema form is unsupported)",
                other,
            ));
        }
    };

    let items = match obj.get("items") {
        None => None,
        Some(v @ (Value::Object(_) | Value::Bool(_))) => {
            Some(Box::new(compile(v, &format!("{schema_path}/items"))?))
        }
        Some(other) => {
            return Err(wrong_type(
                schema_path,
                "items",
                "a schema (object or boolean); per-position arrays are unsupported",
                other,
            ));
        }
    };

    let pattern = match obj.get("pattern") {
        None => None,
        Some(Value::String(s)) => Some(compile_regex(s, schema_path, "pattern")?),
        Some(other) => {
            return Err(wrong_type(schema_path, "pattern", "a string", other));
        }
    };

    Ok(Node {
        schema_path: schema_path.to_string(),
        types,
        enum_values,
        const_value,
        all_of: parse_subschema_array(obj, "allOf", schema_path)?,
        any_of: parse_subschema_array(obj, "anyOf", schema_path)?,
        one_of: parse_subschema_array(obj, "oneOf", schema_path)?,
        not: parse_subschema(obj, "not", schema_path)?,
        if_schema: parse_subschema(obj, "if", schema_path)?,
        then_schema: parse_subschema(obj, "then", schema_path)?,
        else_schema: parse_subschema(obj, "else", schema_path)?,
        properties,
        required,
        additional_properties_forbidden,
        items,
        min_items: parse_usize(obj.get("minItems"), schema_path, "minItems")?,
        max_items: parse_usize(obj.get("maxItems"), schema_path, "maxItems")?,
        minimum: parse_number(obj.get("minimum"), schema_path, "minimum")?,
        maximum: parse_number(obj.get("maximum"), schema_path, "maximum")?,
        min_length: parse_usize(obj.get("minLength"), schema_path, "minLength")?,
        max_length: parse_usize(obj.get("maxLength"), schema_path, "maxLength")?,
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
        Some(other) => Err(wrong_type(
            schema_path,
            key,
            "a schema (object or boolean)",
            other,
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
                out.push(compile(item, &format!("{schema_path}/{key}/{i}"))?);
            }
            Ok(Some(out))
        }
        Some(Value::Array(_)) => Err(parse_err(
            schema_path,
            format!("`{key}` must contain at least one schema"),
        )),
        Some(other) => Err(wrong_type(
            schema_path,
            key,
            "a non-empty array of schemas",
            other,
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
        Some(other) => Err(wrong_type(schema_path, key, "a number", other)),
    }
}

fn parse_usize(
    v: Option<&Value>,
    schema_path: &str,
    key: &str,
) -> Result<Option<usize>, SchemaParseError> {
    match v {
        None => Ok(None),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(u) => Ok(Some(u as usize)),
            None => Err(parse_err(
                schema_path,
                format!("`{key}` must be a non-negative integer, got {n}"),
            )),
        },
        Some(other) => Err(wrong_type(
            schema_path,
            key,
            "a non-negative integer",
            other,
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

/// The parse error for a keyword holding the wrong kind of value.
fn wrong_type(schema_path: &str, key: &str, expected: &str, got: &Value) -> SchemaParseError {
    parse_err(
        schema_path,
        format!("`{key}` must be {expected}, got {}", value_label(got)),
    )
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

// Checking

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
                let mut message = format!(
                    "expected type {}, got {}",
                    join_or(&labels),
                    value_label(instance)
                );
                if let Some(hint) = retype_hint(types, instance) {
                    message.push_str(&format!(": {hint}"));
                }
                self.violation(instance_path, "/type", message, out);
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

        // Only allOf writes each branch's violations into `out`: every branch
        // must hold, so all of them reach the agent.
        if let Some(schemas) = &self.all_of {
            for sub in schemas {
                sub.check(instance, instance_path, out);
            }
        }

        if let Some(schemas) = &self.any_of {
            if !schemas.iter().any(|sub| sub.accepts(instance)) {
                self.violation(
                    instance_path,
                    "/anyOf",
                    "value does not match any of the anyOf schemas",
                    out,
                );
            }
        }

        if let Some(schemas) = &self.one_of {
            let count = schemas.iter().filter(|sub| sub.accepts(instance)).count();
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
            if sub.accepts(instance) {
                self.violation(
                    instance_path,
                    "/not",
                    "value must not match the `not` schema",
                    out,
                );
            }
        }

        if let Some(if_sub) = &self.if_schema {
            let branch = if if_sub.accepts(instance) {
                &self.then_schema
            } else {
                &self.else_schema
            };
            if let Some(sub) = branch {
                sub.check(instance, instance_path, out);
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

    /// Whether `instance` satisfies this node, with the violations discarded.
    fn accepts(&self, instance: &Value) -> bool {
        let mut violations = Vec::new();
        self.check(instance, "", &mut violations);
        violations.is_empty()
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

        if self.additional_properties_forbidden {
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

/// The change to suggest when a type is wrong only because the model quoted the
/// value. Reached for what no retype recovered, since the report is written
/// from what the retype produced.
fn retype_hint(types: &[JsonType], instance: &Value) -> Option<&'static str> {
    let unquoted = types
        .iter()
        .any(|t| matches!(t, JsonType::Integer | JsonType::Number | JsonType::Boolean));
    let structured = types
        .iter()
        .any(|t| matches!(t, JsonType::Object | JsonType::Array));
    match instance {
        Value::String(_) if unquoted => Some("send the value unquoted"),
        // A structure written as text is decoded when it parses, so what
        // reaches here is text that did not.
        Value::String(_) if structured => Some("send it as JSON, not as a string"),
        Value::Number(_) | Value::Bool(_) if types.contains(&JsonType::String) => {
            Some("send the value quoted")
        }
        _ => None,
    }
}

// Retyping

impl Node {
    /// Retype the values this schema names a type for, recording each rewrite
    /// and where it happened. Runs only on a value that already failed, and the
    /// caller checks what it produced.
    ///
    /// Each step reads a value the steps before it are done with: this node's
    /// own shape, then its children, then the subschemas that select on what
    /// those produced. So an `if` reads a discriminator already folded to its
    /// declared spelling. A branch of `anyOf` or `oneOf` retypes nothing:
    /// choosing a value's shape there would also choose the branch, so the
    /// violations name the mismatch instead.
    fn coerce(&self, value: &mut Value, instance_path: &str, out: &mut Vec<String>) {
        if let Some(types) = self.types.as_deref() {
            if !types.iter().any(|t| t.matches(value)) {
                if let Some(retyped) = types.iter().find_map(|t| t.retype(value)) {
                    *value = retyped;
                    out.push(instance_path.to_string());
                }
            }
        }

        // Outside the block above: an `enum` with no declared `type` never
        // reaches it, and the value the model wrote is still recoverable.
        if let Some(declared) = self.enum_candidate(value) {
            *value = declared;
            out.push(instance_path.to_string());
        }

        // Recurse once the value holds its final shape. `additionalProperties`
        // carries no subschema here and `items` has no tuple form, so a
        // property and an element are the only children to reach.
        match value {
            Value::Object(map) => {
                for (name, sub) in self.properties.iter().flatten() {
                    if let Some(field) = map.get_mut(name) {
                        let child_path = format!("{instance_path}/{}", escape_pointer(name));
                        sub.coerce(field, &child_path, out);
                    }
                }
            }
            Value::Array(items) => {
                if let Some(items_schema) = &self.items {
                    for (i, item) in items.iter_mut().enumerate() {
                        let child_path = format!("{instance_path}/{i}");
                        items_schema.coerce(item, &child_path, out);
                    }
                }
            }
            _ => {}
        }

        for branch in self.all_of.iter().flatten() {
            branch.coerce(value, instance_path, out);
        }

        // Once selected, a branch is part of the schema, so its rewrites are
        // this node's own.
        if let Some(if_sub) = &self.if_schema {
            let selected = if if_sub.accepts(value) {
                &self.then_schema
            } else {
                &self.else_schema
            };
            if let Some(sub) = selected {
                sub.coerce(value, instance_path, out);
            }
        }
    }

    /// The one declared value `value` names, read without case or padding.
    /// Two candidates naming it means the spelling picks neither, so nothing
    /// is rewritten.
    fn enum_candidate(&self, value: &Value) -> Option<Value> {
        let candidates = self.enum_values.as_ref()?;
        if candidates.iter().any(|candidate| candidate == value) {
            return None;
        }
        let written = text_form(value);
        let mut named = candidates
            .iter()
            .filter(|candidate| text_form(candidate) == written);
        let candidate = named.next()?;
        named.next().is_none().then(|| candidate.clone())
    }
}

/// The text a value reads as, folded so a capitalised or padded spelling still
/// names the declared one.
fn text_form(value: &Value) -> String {
    match value {
        Value::String(text) => text.trim().to_lowercase(),
        other => other.to_string(),
    }
}

impl JsonType {
    /// The value this type would accept, when the rewrite is exact. `None`
    /// leaves the value for the violation report to name.
    ///
    /// Every arm reads a quoted literal back to what the model wrote; nothing
    /// wraps or stringifies, which would pick a shape it never wrote.
    /// `null` is never produced: for a nullable string `"null"` is a real value.
    fn retype(self, value: &Value) -> Option<Value> {
        match (self, value) {
            (Self::Integer, Value::String(text)) => retype_integer(text),
            (Self::Number, Value::String(text)) => retype_number(text),
            (Self::Boolean, Value::String(text)) => retype_boolean(text),
            (Self::Object, Value::String(text)) => decode_json(text, Value::is_object),
            (Self::Array, Value::String(text)) => decode_json(text, Value::is_array),
            _ => None,
        }
    }
}

fn retype_integer(text: &str) -> Option<Value> {
    let text = text.trim();
    if let Ok(number) = text.parse::<i64>() {
        return Some(Value::from(number));
    }
    // `"42.0"` and `"1e3"` are the same slip. Past 2^53 a float no longer names
    // one integer, so the rewrite stops there.
    let number = text.parse::<f64>().ok()?;
    let exact = number.is_finite() && number.fract() == 0.0 && number.abs() <= (1u64 << 53) as f64;
    exact.then(|| Value::from(number as i64))
}

fn retype_number(text: &str) -> Option<Value> {
    // Rust parses `"inf"` and `"NaN"`, and `Value::from` turns both into null.
    let number = text.trim().parse::<f64>().ok()?;
    number.is_finite().then(|| Value::from(number))
}

fn retype_boolean(text: &str) -> Option<Value> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        _ => None,
    }
}

/// Decode a structure the model wrote as JSON text. Length is not capped: the
/// longest results are the reports an agent is most likely to double-encode,
/// and a cap would reject one with a violation that never names the size.
fn decode_json(text: &str, fits: fn(&Value) -> bool) -> Option<Value> {
    let decoded = serde_json::from_str::<Value>(text).ok()?;
    fits(&decoded).then_some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The value `schema` keeps, for the tests that do not read the pointers.
    fn kept(schema: &Schema, value: Value) -> Value {
        schema.validate(value).unwrap().0
    }

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

    #[test]
    fn validate_reads_an_enum_value_the_model_capitalised() {
        let schema = Schema::new(json!({"type": "string", "enum": ["content", "count"]})).unwrap();
        assert_eq!(kept(&schema, json!("Content")), json!("content"));
    }

    #[test]
    fn validate_reads_an_enum_value_the_model_padded() {
        let schema = Schema::new(json!({"type": "string", "enum": ["content"]})).unwrap();
        assert_eq!(kept(&schema, json!(" content ")), json!("content"));
    }

    #[test]
    fn validate_leaves_a_value_two_enum_candidates_read_as_alone() {
        // Neither spelling is the one meant, so the violation names the value.
        let schema = Schema::new(json!({"enum": ["draft", "Draft"]})).unwrap();
        assert!(schema.validate(json!("DRAFT")).is_err());
    }

    #[test]
    fn validate_reads_an_enum_value_with_no_declared_type() {
        let schema = Schema::new(json!({"enum": ["open", "closed"]})).unwrap();
        assert_eq!(kept(&schema, json!("OPEN")), json!("open"));
    }

    #[test]
    fn validate_names_the_pointer_of_an_enum_it_read() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": {"mode": {"enum": ["content"]}},
        }))
        .unwrap();
        let (_, repaired) = schema.validate(json!({"mode": "Content"})).unwrap();
        assert_eq!(repaired, vec!["/mode"]);
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
        // Neither value is recoverable, so both violations survive the retype.
        let violations = schema
            .validate(json!({"name": {"a": 1}, "age": -1}))
            .unwrap_err();
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

    /// An object whose `action` decides which other fields are required, the
    /// shape every tool taking an `action` argument declares.
    fn discriminated_schema() -> Schema {
        Schema::new(json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["write", "read"] },
                "slug": { "type": "string" },
                "content": { "type": "string" },
            },
            "required": ["action"],
            "allOf": [
                {
                    "if": {
                        "required": ["action"],
                        "properties": { "action": { "const": "write" } },
                    },
                    "then": { "required": ["slug", "content"] },
                },
                {
                    "if": {
                        "required": ["action"],
                        "properties": { "action": { "const": "read" } },
                    },
                    "then": { "required": ["slug"] },
                },
            ],
        }))
        .unwrap()
    }

    fn messages(violations: &SchemaViolations) -> Vec<&str> {
        violations.iter().map(|v| v.message.as_str()).collect()
    }

    #[test]
    fn validate_requires_only_the_fields_the_discriminator_selects() {
        let schema = discriminated_schema();
        assert!(schema
            .validate(json!({"action": "read", "slug": "s"}))
            .is_ok());
        let violations = schema
            .validate(json!({"action": "write", "slug": "s"}))
            .unwrap_err();
        assert_eq!(
            messages(&violations),
            vec!["missing required property `content`"]
        );
    }

    #[test]
    fn validate_names_every_field_one_branch_requires_at_once() {
        // One report, so a value missing three fields costs one retry.
        let violations = discriminated_schema()
            .validate(json!({"action": "write"}))
            .unwrap_err();
        assert_eq!(
            messages(&violations),
            vec![
                "missing required property `slug`",
                "missing required property `content`",
            ]
        );
    }

    #[test]
    fn validate_selects_a_branch_from_a_discriminator_it_read() {
        // `Write` folds to `write` before the branch is picked, so the report
        // names what that branch requires rather than the spelling alone.
        let violations = discriminated_schema()
            .validate(json!({"action": "Write"}))
            .unwrap_err();
        assert_eq!(
            messages(&violations),
            vec![
                "missing required property `slug`",
                "missing required property `content`",
            ]
        );
    }

    #[test]
    fn validate_retypes_a_value_under_the_branch_it_selected() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "mode": { "type": "string" } },
            "if": {
                "required": ["mode"],
                "properties": { "mode": { "const": "count" } },
            },
            "then": { "properties": { "limit": { "type": "integer" } } },
        }))
        .unwrap();
        assert_eq!(
            kept(&schema, json!({"mode": "count", "limit": "5"})),
            json!({"mode": "count", "limit": 5})
        );
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
        assert_eq!(schema.validate(value.clone()).unwrap(), (value, vec![]));
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
        let (decoded, _) = schema.validate(encoded).unwrap();
        assert_eq!(decoded, json!({"status": "ok"}));
    }

    #[test]
    fn validate_decodes_a_long_string_encoded_object() {
        // A long report is the one an agent is most likely to double-encode.
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "details": { "type": "string" } },
            "required": ["details"],
        }))
        .unwrap();
        let details = "d".repeat(100_000);
        let encoded = serde_json::to_string(&json!({ "details": details })).unwrap();
        assert_eq!(kept(&schema, json!(encoded)), json!({ "details": details }));
    }

    #[test]
    fn validate_reports_what_a_decoded_object_still_fails() {
        // The string decodes to a valid object that still fails `required`.
        // What the retry has to fix is the missing property, not the quoting
        // the decode already saw through.
        let schema = Schema::new(json!({"type": "object", "required": ["status"]})).unwrap();
        let violations = schema.validate(json!("{\"other\": 1}")).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| v.schema_path.ends_with("/required")));
        assert!(!violations.iter().any(|v| v.schema_path.ends_with("/type")));
    }

    #[test]
    fn validate_non_string_fails_type_check_without_decode_attempt() {
        let schema = Schema::new(json!({"type": "object", "required": ["status"]})).unwrap();
        assert!(schema.validate(json!(42)).is_err());
    }

    // Retyping

    fn line_schema() -> Schema {
        Schema::new(json!({
            "type": "object",
            "properties": { "line": { "type": "integer" } },
            "required": ["line"],
        }))
        .unwrap()
    }

    #[test]
    fn validate_retypes_a_quoted_integer() {
        assert_eq!(
            kept(&line_schema(), json!({"line": "42"})),
            json!({"line": 42})
        );
    }

    #[test]
    fn validate_retypes_a_quoted_number() {
        let schema = Schema::new(json!({"type": "number"})).unwrap();
        assert_eq!(kept(&schema, json!("2.5")), json!(2.5));
    }

    #[test]
    fn validate_retypes_a_quoted_boolean() {
        let schema = Schema::new(json!({"type": "boolean"})).unwrap();
        assert_eq!(kept(&schema, json!("true")), json!(true));
    }

    #[test]
    fn validate_retypes_a_quoted_whole_float_to_an_integer() {
        assert_eq!(
            kept(&line_schema(), json!({"line": "42.0"})),
            json!({"line": 42})
        );
    }

    #[test]
    fn validate_leaves_a_quoted_integer_with_decimals_alone() {
        assert!(line_schema().validate(json!({"line": "42.5"})).is_err());
    }

    #[test]
    fn validate_leaves_an_integer_beyond_exact_float_range_alone() {
        // Past 2^53 a float names a range of integers, not one to rewrite to.
        assert!(line_schema().validate(json!({"line": "1e300"})).is_err());
    }

    #[test]
    fn validate_leaves_a_non_finite_number_string_alone() {
        // Rust parses "inf", and `Value::from` turns a non-finite float into
        // null, which this nullable schema would then accept.
        let schema = Schema::new(json!({"type": ["number", "null"]})).unwrap();
        assert!(schema.validate(json!("inf")).is_err());
    }

    #[test]
    fn validate_leaves_the_text_null_alone() {
        // No retype produces null, so the text stays a value in its own right.
        let schema = Schema::new(json!({"type": "null"})).unwrap();
        assert!(schema.validate(json!("null")).is_err());
    }

    #[test]
    fn validate_decodes_a_nested_json_string_into_an_array() {
        let schema = Schema::new(json!({
            "type": "object",
            "properties": { "lines": { "type": "array", "items": { "type": "integer" } } },
            "required": ["lines"],
        }))
        .unwrap();
        assert_eq!(
            kept(&schema, json!({"lines": "[1, 2]"})),
            json!({"lines": [1, 2]})
        );
    }

    #[test]
    fn validate_retypes_each_element_of_an_array() {
        let schema = Schema::new(json!({"type": "array", "items": {"type": "integer"}})).unwrap();
        assert_eq!(kept(&schema, json!(["1", "2"])), json!([1, 2]));
    }

    #[test]
    fn validate_never_rewrites_a_value_to_a_shape_the_model_did_not_write() {
        // Stringifying the number or wrapping the lone value would pass the
        // check with a value the model never wrote; the violation asks for the
        // right shape instead.
        let string = Schema::new(json!({"type": "string"})).unwrap();
        let violations = string.validate(json!(42)).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| v.message.contains("send the value quoted")));
        let array = Schema::new(json!({"type": "array", "items": {"type": "string"}})).unwrap();
        assert!(array.validate(json!("urgent")).is_err());
    }

    #[test]
    fn validate_does_not_retype_inside_a_union_branch() {
        // Reading `"42"` as the integer branch would also choose the branch,
        // so the violation names the mismatch instead.
        let schema = Schema::new(json!({
            "anyOf": [{ "type": "integer" }, { "type": "boolean" }],
        }))
        .unwrap();
        assert!(schema.validate(json!("42")).is_err());
    }

    #[test]
    fn validate_reports_only_what_a_retype_could_not_recover() {
        // `line` retypes but `name` cannot, so the whole value still fails.
        // Only `name` is reported: it is the one thing a retry has to change.
        let schema = Schema::new(json!({
            "type": "object",
            "properties": {
                "line": { "type": "integer" },
                "name": { "type": "string", "minLength": 3 },
            },
            "required": ["line", "name"],
        }))
        .unwrap();
        let violations = schema
            .validate(json!({"line": "42", "name": "ab"}))
            .unwrap_err();
        let paths: Vec<&str> = violations
            .iter()
            .map(|v| v.instance_path.as_str())
            .collect();
        assert_eq!(paths, vec!["/name"]);
    }

    #[test]
    fn validate_retypes_inside_a_decoded_string() {
        assert_eq!(
            kept(&line_schema(), json!("{\"line\": \"42\"}")),
            json!({"line": 42})
        );
    }

    #[test]
    fn validate_names_the_pointer_it_repaired() {
        let (_, repaired) = line_schema().validate(json!({"line": "42"})).unwrap();
        assert_eq!(repaired, vec!["/line"]);
    }

    #[test]
    fn validate_reports_a_decode_before_the_retype_it_enabled() {
        // The decode is the root's own retype, so it carries the empty pointer.
        let (_, repaired) = line_schema().validate(json!("{\"line\": \"42\"}")).unwrap();
        assert_eq!(repaired, vec!["", "/line"]);
    }

    #[test]
    fn validate_reports_nothing_for_a_conforming_value() {
        let (_, repaired) = line_schema().validate(json!({"line": 42})).unwrap();
        assert!(repaired.is_empty());
    }

    #[test]
    fn type_violation_names_the_quoting_to_change() {
        let violations = line_schema()
            .validate(json!({"line": "about 42"}))
            .unwrap_err();
        assert!(violations
            .iter()
            .any(|v| v.message.contains("send the value unquoted")));
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
