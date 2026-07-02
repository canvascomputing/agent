//! One place that decides how a finish tool's arguments carry the ticket result,
//! so the shape the request builder advertises and the shape the tool reads can
//! never disagree. A ticket with an object schema is *inlined* (its fields are
//! the finish tool's top-level arguments); a scalar, array, or absent schema stays
//! *enveloped* under `result`, since those cannot be a tool call's arguments.

use serde_json::Value;

use crate::schemas::Schema;

pub(crate) enum ResultShape {
    Inlined,
    Enveloped,
}

/// Top-level keys a finish tool owns; stripped from the arguments to recover the
/// result. A result schema must not reuse these names: `handover_ticket` would
/// mistake such a field for a control key. No schema in the tree does.
fn control_keys(tool_name: &str) -> &'static [&'static str] {
    match tool_name {
        "handover_ticket" => &["to", "task", "schema"],
        _ => &[],
    }
}

/// Object schema means the arguments are the result object; anything else keeps
/// the `result` envelope.
pub(crate) fn result_shape(schema: Option<&Schema>) -> ResultShape {
    let is_object = schema
        .and_then(|s| serde_json::to_value(s).ok())
        .and_then(|document| {
            document
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some("object");
    if is_object {
        ResultShape::Inlined
    } else {
        ResultShape::Enveloped
    }
}

/// Recover the result value from a finish tool call's arguments. Callers still read
/// their own control keys (`to`/`task`/`schema`) off `input` separately.
pub(crate) fn parse_result(tool_name: &str, schema: Option<&Schema>, input: &Value) -> Value {
    match result_shape(schema) {
        ResultShape::Enveloped => input.get("result").cloned().unwrap_or(Value::Null),
        ResultShape::Inlined => {
            let mut object = input.as_object().cloned().unwrap_or_default();
            for key in control_keys(tool_name) {
                object.remove(*key);
            }
            unwrap_accidental_result(Value::Object(object), schema)
        }
    }
}

/// A model that wrapped `{"result": <value>}` despite the flat schema is
/// unwrapped, unless the schema itself declares a `result` property.
fn unwrap_accidental_result(value: Value, schema: Option<&Schema>) -> Value {
    if let Value::Object(map) = &value {
        if map.len() == 1 {
            if let Some(inner) = map.get("result") {
                if !declares_property(schema, "result") {
                    return inner.clone();
                }
            }
        }
    }
    value
}

/// The model-facing arguments schema for a finish tool. Inlined: the object schema
/// (with `handover_ticket`'s control keys merged back in). Enveloped: the tool's
/// static schema with the ticket schema grafted onto its `result` property.
pub(crate) fn finish_tool_input_schema(
    tool_name: &str,
    static_schema: Value,
    schema: Option<&Schema>,
) -> Value {
    match (result_shape(schema), schema) {
        (ResultShape::Inlined, Some(schema)) => {
            let document = serde_json::to_value(schema).unwrap_or_else(|_| static_schema.clone());
            if control_keys(tool_name).is_empty() {
                document
            } else {
                merge_controls(document, &static_schema, tool_name)
            }
        }
        (ResultShape::Enveloped, Some(schema)) => graft_onto_result(static_schema, schema),
        _ => static_schema,
    }
}

/// Set the `result` property of the tool's static schema to the ticket schema,
/// so an enveloped finish tool advertises a typed `result` instead of "any value".
fn graft_onto_result(mut static_schema: Value, schema: &Schema) -> Value {
    if let Ok(document) = serde_json::to_value(schema) {
        if let Some(result) = static_schema
            .get_mut("properties")
            .and_then(|properties| properties.get_mut("result"))
        {
            *result = document;
        }
    }
    static_schema
}

/// Add the finish tool's control-key property definitions and required entries
/// (taken from its static schema) onto the inlined object schema.
fn merge_controls(mut document: Value, static_schema: &Value, tool_name: &str) -> Value {
    let static_properties = static_schema.get("properties").and_then(Value::as_object);
    let properties = document.as_object_mut().and_then(|object| {
        object
            .entry("properties")
            .or_insert_with(|| Value::Object(Default::default()))
            .as_object_mut()
    });
    if let (Some(properties), Some(static_properties)) = (properties, static_properties) {
        for key in control_keys(tool_name) {
            if let Some(definition) = static_properties.get(*key) {
                properties.insert((*key).to_string(), definition.clone());
            }
        }
    }
    if let Some(required) = document.as_object_mut().and_then(|object| {
        object
            .entry("required")
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
    }) {
        for key in control_keys(tool_name) {
            if *key != "schema" {
                required.push(Value::String((*key).to_string()));
            }
        }
    }
    document
}

fn declares_property(schema: Option<&Schema>, name: &str) -> bool {
    schema
        .and_then(|s| serde_json::to_value(s).ok())
        .and_then(|document| {
            document
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| properties.contains_key(name))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object_schema() -> Schema {
        Schema::parse(json!({
            "type": "object",
            "properties": { "status": { "type": "string" }, "note": { "type": "string" } },
            "required": ["status"],
        }))
        .unwrap()
    }

    fn string_schema() -> Schema {
        Schema::parse(json!({ "type": "string" })).unwrap()
    }

    #[test]
    fn object_schema_is_inlined_others_enveloped() {
        assert!(matches!(
            result_shape(Some(&object_schema())),
            ResultShape::Inlined
        ));
        assert!(matches!(
            result_shape(Some(&string_schema())),
            ResultShape::Enveloped
        ));
        assert!(matches!(result_shape(None), ResultShape::Enveloped));
    }

    #[test]
    fn finish_inlined_takes_the_whole_arguments_object() {
        let input = json!({ "status": "malicious", "note": "x" });
        let result = parse_result("finish_ticket", Some(&object_schema()), &input);
        assert_eq!(result, json!({ "status": "malicious", "note": "x" }));
    }

    #[test]
    fn finish_inlined_unwraps_an_accidental_result_wrapper() {
        let input = json!({ "result": { "status": "malicious" } });
        let result = parse_result("finish_ticket", Some(&object_schema()), &input);
        assert_eq!(result, json!({ "status": "malicious" }));
    }

    #[test]
    fn a_schema_that_declares_result_is_not_unwrapped() {
        let schema = Schema::parse(json!({
            "type": "object",
            "properties": { "result": { "type": "string" } },
            "required": ["result"],
        }))
        .unwrap();
        let input = json!({ "result": "the answer" });
        let result = parse_result("finish_ticket", Some(&schema), &input);
        assert_eq!(result, json!({ "result": "the answer" }));
    }

    #[test]
    fn handover_inlined_strips_control_keys() {
        let input = json!({ "status": "malicious", "to": "analysis", "task": "go" });
        let result = parse_result("handover_ticket", Some(&object_schema()), &input);
        assert_eq!(result, json!({ "status": "malicious" }));
    }

    #[test]
    fn enveloped_reads_the_result_field() {
        let input = json!({ "to": "a", "task": "t", "result": "the overview" });
        let result = parse_result("handover_ticket", Some(&string_schema()), &input);
        assert_eq!(result, json!("the overview"));
    }

    #[test]
    fn finish_inlined_schema_is_the_object_schema() {
        let advertised = finish_tool_input_schema(
            "finish_ticket",
            json!({ "type": "object", "properties": { "result": {} } }),
            Some(&object_schema()),
        );
        assert_eq!(
            advertised["properties"]["status"],
            json!({ "type": "string" })
        );
        assert!(advertised["properties"].get("result").is_none());
    }

    #[test]
    fn handover_inlined_schema_has_result_fields_flat_plus_control_keys() {
        let static_schema = json!({
            "type": "object",
            "properties": {
                "to": { "type": "string" },
                "task": { "type": "string" },
                "schema": { "type": "object" },
                "result": {},
            },
            "required": ["to", "task", "result"],
        });
        let advertised =
            finish_tool_input_schema("handover_ticket", static_schema, Some(&object_schema()));
        assert_eq!(
            advertised["properties"]["status"],
            json!({ "type": "string" })
        );
        assert_eq!(advertised["properties"]["to"], json!({ "type": "string" }));
        assert!(advertised["properties"].get("result").is_none());
        let required = advertised["required"].as_array().unwrap();
        assert!(required.contains(&json!("to")) && required.contains(&json!("task")));
    }

    #[test]
    fn enveloped_schema_grafts_the_ticket_schema_onto_result() {
        let static_schema = json!({
            "type": "object",
            "properties": { "result": { "description": "any value" } },
        });
        let advertised =
            finish_tool_input_schema("finish_ticket", static_schema, Some(&string_schema()));
        assert_eq!(
            advertised["properties"]["result"],
            json!({ "type": "string" })
        );
    }
}
