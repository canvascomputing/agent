//! Exposes directive keys and text selection through Python.

use agentwerk::Directive;
use pyo3::prelude::*;

/// Every directive agentwerk can send, one constant per key.
///
/// `Agent.directives(compute)` takes the function deciding all of them. Match
/// the key it hands you against these constants, and return `None` for the ones
/// you leave as they are.
#[pyclass(name = "Directive")]
pub struct PyDirective;

/// Turn a Python function into the one an agent takes. A call that raises
/// prints its traceback and keeps the catalogue text, since a directive is
/// already agentwerk answering a failure.
pub(crate) fn compute(
    compute: Py<PyAny>,
) -> impl Fn(&str) -> Option<String> + Send + Sync + 'static {
    move |key: &str| {
        Python::attach(|py| match compute.bind(py).call1((key,)) {
            Ok(returned) => returned.extract::<Option<String>>().unwrap_or(None),
            Err(error) => {
                error.print(py);
                None
            }
        })
    }
}

/// Register the class, and every key as a constant on it so the two languages
/// spell them alike.
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyDirective>()?;
    let directive = module.getattr("Directive")?;
    for key in Directive::ALL {
        directive.setattr(key.to_uppercase().as_str(), *key)?;
    }
    Ok(())
}
