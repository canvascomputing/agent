//! The knowledge store as Python sees it. Open one, limit its index, and hand it
//! to one or more agents so they share what they learn.
//!
//! `pages()` is how you write those pages yourself, rather than leaving them to
//! the agent.

use std::sync::Arc;

use agentwerk::agents::knowledge::{Page, Pages};
use agentwerk::Knowledge;
use pyo3::prelude::*;

use crate::convert::runtime_error;

/// `Knowledge` allows agents to share insights or learnings, kept on disk under
/// one directory.
#[pyclass(name = "Knowledge")]
pub struct PyKnowledge {
    pub inner: Arc<Knowledge>,
}

#[pymethods]
impl PyKnowledge {
    /// Open a knowledge store at `dir/knowledge`, or seed one from the pages
    /// already there.
    #[staticmethod]
    fn load(dir: &str) -> PyResult<Self> {
        let inner = Knowledge::load(dir).map_err(runtime_error)?;
        Ok(PyKnowledge { inner })
    }

    /// Limit how much of the index is injected into the prompt, in characters.
    /// No write is ever refused for being too large.
    fn index_char_limit<'py>(slf: PyRef<'py, Self>, n: usize) -> PyRef<'py, Self> {
        slf.inner.index_char_limit(n);
        slf
    }

    /// Get the index size limit in force, 12 000 until it is changed.
    fn get_index_char_limit(&self) -> usize {
        self.inner.get_index_char_limit()
    }

    /// Get the index, which is injected into the agent prompt.
    fn index(&self) -> String {
        self.inner.index()
    }

    /// Get the page collection for reading and writing pages.
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

/// The page collection of one store. Python holds a shared handle, so it
/// outlives the expression that produced it.
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
    /// Create or replace a page, and its entry in the index.
    fn save(&self, page: PyRef<'_, PyPage>) -> PyResult<()> {
        self.pages().save(page.to_page()).map_err(runtime_error)
    }

    /// Read one page by its slug. Raises when there is none.
    fn load(&self, slug: &str) -> PyResult<PyPage> {
        let page = self.pages().load(slug).map_err(runtime_error)?;
        Ok(PyPage { inner: page })
    }

    /// Get every page in the store, in index order.
    fn list(&self) -> PyResult<Vec<PyPage>> {
        let pages = self.pages().list().map_err(runtime_error)?;
        Ok(pages.into_iter().map(|inner| PyPage { inner }).collect())
    }

    /// Remove one page and its entry in the index.
    fn remove(&self, slug: &str) -> PyResult<()> {
        self.pages().remove(slug).map_err(runtime_error)
    }
}

/// A `Page` is one thing an agent learned: a slug, a one-line description, a
/// markdown body, and tags.
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
    /// What kind of page this is.
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
