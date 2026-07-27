//! The cross-ticket knowledge store as Python sees it. Build one store, cap its
//! rendered index, and bind it to one or more agents so they share durable
//! facts across every ticket and across runs. `pages()` is the seam for curating
//! those facts from Python instead of leaving them to the agent.

use std::sync::Arc;

use agentwerk::agents::knowledge::{Page, Pages};
use agentwerk::Knowledge;
use pyo3::prelude::*;

use crate::convert::runtime_error;

/// A shared knowledge store rooted at a directory.
#[pyclass(name = "Knowledge")]
pub struct PyKnowledge {
    pub inner: Arc<Knowledge>,
}

#[pymethods]
impl PyKnowledge {
    /// Open (or seed from) an Open Knowledge Format bundle at `dir/knowledge`.
    #[staticmethod]
    fn load(dir: &str) -> PyResult<Self> {
        let inner = Knowledge::load(dir).map_err(runtime_error)?;
        Ok(PyKnowledge { inner })
    }

    /// Cap the rendered index injected into the system prompt, in characters.
    fn index_char_limit<'py>(slf: PyRef<'py, Self>, n: usize) -> PyRef<'py, Self> {
        Arc::clone(&slf.inner).index_char_limit(n);
        slf
    }

    /// The current rendered index (the bullet list), or an empty string.
    fn index(&self) -> String {
        self.inner.index()
    }

    /// The page collection, for curating the store directly.
    fn pages(&self) -> PyPages {
        PyPages {
            store: Arc::clone(&self.inner),
        }
    }

    /// Remove every page from the store.
    fn clear(&self) -> PyResult<()> {
        self.inner.clear().map_err(runtime_error)
    }
}

/// The page collection of one store. Rust borrows the store for the duration of
/// the call; Python holds a shared handle instead, so the object outlives the
/// expression that produced it.
#[pyclass(name = "Pages")]
pub struct PyPages {
    store: Arc<Knowledge>,
}

impl PyPages {
    fn pages(&self) -> Pages<'_> {
        self.store.pages()
    }
}

#[pymethods]
impl PyPages {
    /// Create or replace a page and its index entry.
    fn save(&self, page: PyRef<'_, PyPage>) -> PyResult<()> {
        self.pages().save(page.to_page()).map_err(runtime_error)
    }

    /// The page stored under `slug`. Raises when there is none.
    fn load(&self, slug: &str) -> PyResult<PyPage> {
        let page = self.pages().load(slug).map_err(runtime_error)?;
        Ok(PyPage { inner: page })
    }

    /// Drop a page and its index entry.
    fn remove(&self, slug: &str) -> PyResult<()> {
        self.pages().remove(slug).map_err(runtime_error)
    }
}

/// One knowledge page: a concept document with a slug, a one-line description,
/// a markdown body, and tags.
#[pyclass(name = "Page")]
pub struct PyPage {
    inner: Page,
}

impl PyPage {
    fn to_page(&self) -> Page {
        self.inner.clone()
    }
}

#[pymethods]
impl PyPage {
    /// `kind` names the concept type.
    #[new]
    #[pyo3(signature = (slug, description, content, kind="Knowledge", tags=None))]
    fn new(
        slug: String,
        description: String,
        content: String,
        kind: &str,
        tags: Option<Vec<String>>,
    ) -> Self {
        PyPage {
            inner: Page {
                slug,
                kind: kind.to_string(),
                description,
                content,
                tags: tags.unwrap_or_default(),
            },
        }
    }

    #[getter]
    fn slug(&self) -> &str {
        &self.inner.slug
    }

    #[getter]
    fn kind(&self) -> &str {
        &self.inner.kind
    }

    #[getter]
    fn description(&self) -> &str {
        &self.inner.description
    }

    #[getter]
    fn content(&self) -> &str {
        &self.inner.content
    }

    #[getter]
    fn tags(&self) -> Vec<String> {
        self.inner.tags.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "Page(slug={:?}, kind={:?})",
            self.inner.slug, self.inner.kind
        )
    }
}
