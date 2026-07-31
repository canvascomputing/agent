//! One section of a prompt: its markdown body, and the heading the builder puts
//! above it.

use std::borrow::Cow;

/// One section of an assembled prompt. Knows whether it should render under a
/// `## Heading` (Knowledge) or as bare body (Role, Directive). Keeping
/// this concern here means the source `.md` files hold body content only, and
/// the builder adds the surrounding markdown.
#[derive(Debug, Clone)]
pub(crate) struct Section {
    pub heading: Option<&'static str>,
    pub body: Cow<'static, str>,
}

impl Section {
    pub fn role(body: impl Into<Cow<'static, str>>) -> Self {
        Self {
            heading: None,
            body: body.into(),
        }
    }

    pub fn knowledge(body: impl Into<Cow<'static, str>>) -> Self {
        Self {
            heading: Some("Knowledge"),
            body: body.into(),
        }
    }

    #[allow(dead_code)]
    pub fn task(body: impl Into<Cow<'static, str>>) -> Self {
        Self {
            heading: None,
            body: body.into(),
        }
    }

    #[allow(dead_code)]
    pub fn directive(body: impl Into<Cow<'static, str>>) -> Self {
        Self {
            heading: None,
            body: body.into(),
        }
    }

    /// Trim leading and trailing newlines off the body so the builder controls
    /// section spacing, so blank lines at either end of a source file do not
    /// reach the final prompt.
    pub fn render(&self) -> String {
        let body = self.body.trim_matches('\n');
        match self.heading {
            Some(h) => format!("## {h}\n\n{body}"),
            None => body.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_renders_body_without_heading() {
        let s = Section::role("You are a senior reviewer.");
        assert_eq!(s.render(), "You are a senior reviewer.");
    }

    #[test]
    fn knowledge_wraps_body_in_h2_heading() {
        let s = Section::knowledge("- **config**: Port 8080");
        assert_eq!(s.render(), "## Knowledge\n\n- **config**: Port 8080");
    }

    #[test]
    fn surrounding_newlines_in_body_are_trimmed() {
        let s = Section::role("\n\n- MUST do the thing.\n\n");
        assert_eq!(s.render(), "- MUST do the thing.");
    }
}
