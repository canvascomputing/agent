//! Binds a result contract to a label, so a task picks one up from whichever
//! label routed it to an agent.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::{Schema, SchemaParseError};

/// A `SchemaStore` holds one [`Schema`] per label and hands it to every task
/// claimed under that label that carries no schema of its own.
///
/// Share one store with a queue through `Queue::set_schemas`. A task built
/// with `Task::schema` keeps what it was given.
///
/// ```
/// use agentwerk::SchemaStore;
/// use serde_json::json;
///
/// let schemas = SchemaStore::new();
/// schemas.label("analysis", json!({ "type": "object" }))?;
///
/// assert!(schemas.get("analysis").is_some());
/// assert!(schemas.get("research").is_none());
/// # Ok::<(), agentwerk::schemas::SchemaParseError>(())
/// ```
pub struct SchemaStore {
    entries: Mutex<HashMap<String, Schema>>,
}

impl SchemaStore {
    /// Create an empty store, shared through an `Arc`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
        })
    }

    /// Bind a schema to a label, creating or replacing the entry.
    ///
    /// The document is compiled here, so one that cannot compile fails at
    /// registration rather than at the claim it would have applied to.
    pub fn label(
        &self,
        label: impl Into<String>,
        document: Value,
    ) -> Result<&Self, SchemaParseError> {
        let schema = Schema::new(document)?;
        self.entries.lock().unwrap().insert(label.into(), schema);
        Ok(self)
    }

    /// Read back the schema bound to a label, or `None` when there is none.
    pub fn get(&self, label: &str) -> Option<Schema> {
        self.entries.lock().unwrap().get(label).cloned()
    }
}

impl fmt::Debug for SchemaStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries = self.entries.lock().unwrap();
        f.debug_set().entries(entries.keys()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(title: &str) -> Value {
        json!({ "type": "object", "title": title })
    }

    fn title_of(schema: &Schema) -> String {
        serde_json::to_value(schema).unwrap()["title"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn label_replaces_the_schema_already_bound_to_it() {
        let schemas = SchemaStore::new();
        schemas.label("analysis", document("first")).unwrap();
        assert_eq!(title_of(&schemas.get("analysis").unwrap()), "first");

        schemas.label("analysis", document("second")).unwrap();
        assert_eq!(title_of(&schemas.get("analysis").unwrap()), "second");
    }

    #[test]
    fn get_answers_only_for_the_label_a_schema_was_bound_to() {
        let schemas = SchemaStore::new();
        schemas.label("analysis", document("verdict")).unwrap();
        assert!(schemas.get("research").is_none());
    }

    #[test]
    fn label_rejects_a_document_that_is_not_a_schema() {
        let schemas = SchemaStore::new();
        let error = schemas
            .label("analysis", json!({ "uniqueItems": true }))
            .unwrap_err();
        assert!(error.message.contains("unsupported keyword"));
        assert!(schemas.get("analysis").is_none());
    }
}
