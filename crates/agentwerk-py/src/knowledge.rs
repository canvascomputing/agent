//! The knowledge store as Python sees it. Open one, limit its index, and hand it
//! to one or more agents so they share what they learn.
//!
//! `get_pages()` is how you write those pages yourself, rather than leaving them to
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
    /// Open a knowledge store at `store_dir/knowledge`, or seed one from the
    /// pages already there.
    #[staticmethod]
    fn load(store_dir: &str) -> PyResult<Self> {
        let inner = Knowledge::load(store_dir).map_err(runtime_error)?;
        Ok(PyKnowledge { inner })
    }

    /// Limit how much of the index is injected into the prompt, in characters.
    /// No write is ever refused for being too large.
    fn set_index_char_limit<'py>(slf: PyRef<'py, Self>, count: usize) -> PyRef<'py, Self> {
        slf.inner.set_index_char_limit(count);
        slf
    }

    /// Get the index size limit in force, 12 000 until it is changed.
    fn get_index_char_limit(&self) -> usize {
        self.inner.get_index_char_limit()
    }

    /// Get the index, which is injected into the agent prompt.
    fn get_index(&self) -> String {
        self.inner.get_index()
    }

    /// Get the page collection for reading and writing pages.
    fn get_pages(&self) -> PyPages {
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
    fn collection(&self) -> Pages<'_> {
        self.store.get_pages()
    }
}

#[pymethods]
impl PyPages {
    /// Create or replace a page, and its entry in the index.
    fn save(&self, page: PyRef<'_, PyPage>) -> PyResult<()> {
        self.collection()
            .save(page.to_page())
            .map_err(runtime_error)
    }

    /// Read one page by its slug. Raises when there is none.
    fn get_page(&self, slug: &str) -> PyResult<PyPage> {
        let page = self.collection().get_page(slug).map_err(runtime_error)?;
        Ok(PyPage { inner: page })
    }

    /// Get every page in the store, in index order.
    fn get_all(&self) -> PyResult<Vec<PyPage>> {
        let pages = self.collection().get_all().map_err(runtime_error)?;
        Ok(pages.into_iter().map(|inner| PyPage { inner }).collect())
    }

    /// Remove one page and its entry in the index.
    fn remove(&self, slug: &str) -> PyResult<()> {
        self.collection().remove(slug).map_err(runtime_error)
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

    fn get_slug(&self) -> &str {
        self.inner.get_slug()
    }

    fn get_kind(&self) -> &str {
        self.inner.get_kind()
    }

    fn get_description(&self) -> &str {
        self.inner.get_description()
    }

    fn get_content(&self) -> &str {
        self.inner.get_content()
    }

    fn get_tags(&self) -> Vec<String> {
        self.inner.get_tags().to_vec()
    }

    fn __repr__(&self) -> String {
        format!(
            "Page(slug={:?}, kind={:?})",
            self.inner.get_slug(),
            self.inner.get_kind()
        )
    }
}
