//! Prompt text a setter accepts: the words themselves, or the file holding
//! them.

use std::io;
use std::path::{Path, PathBuf};

/// Text handed to an agent, a tool, or a task.
///
/// A string is the text itself; a path names the file to read. Both arrive
/// trimmed, because a file's closing newline would otherwise reach the model.
#[derive(Debug, Clone)]
pub struct Text(String);

impl Text {
    /// Read the text from a file, reporting a file that cannot be read.
    ///
    /// The conversions panic instead, so this is the door for a host that owns
    /// the failure itself.
    pub fn from_file(file: impl AsRef<Path>) -> io::Result<Self> {
        let file = file.as_ref();
        match std::fs::read_to_string(file) {
            Ok(body) => Ok(Self(body.trim().to_string())),
            Err(error) => Err(io::Error::new(
                error.kind(),
                format!("cannot read `{}`: {error}", file.display()),
            )),
        }
    }

    /// Hand back the trimmed text.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Panics naming the file when it cannot be read: a role or description the
    /// host meant to ship is a misconfiguration, not something the agent should
    /// discover mid-run.
    fn read(file: &Path) -> Self {
        Self::from_file(file).unwrap_or_else(|error| panic!("{error}"))
    }
}

impl From<&str> for Text {
    fn from(text: &str) -> Self {
        Self(text.trim().to_string())
    }
}

impl From<String> for Text {
    fn from(text: String) -> Self {
        Self(text.trim().to_string())
    }
}

impl From<&String> for Text {
    fn from(text: &String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<&Path> for Text {
    fn from(file: &Path) -> Self {
        Self::read(file)
    }
}

impl From<PathBuf> for Text {
    fn from(file: PathBuf) -> Self {
        Self::read(&file)
    }
}

impl From<&PathBuf> for Text {
    fn from(file: &PathBuf) -> Self {
        Self::read(file)
    }
}

impl From<Text> for String {
    fn from(text: Text) -> Self {
        text.into_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn written(dir: &TempDir, body: &str) -> PathBuf {
        let file = dir.path().join("role.md");
        std::fs::write(&file, body).unwrap();
        file
    }

    #[test]
    fn a_string_becomes_the_text_itself() {
        assert_eq!(
            Text::from("You review code.").into_string(),
            "You review code."
        );
    }

    #[test]
    fn a_string_is_trimmed() {
        assert_eq!(
            Text::from("\n\nYou review code.\n").into_string(),
            "You review code."
        );
    }

    #[test]
    fn a_path_becomes_the_trimmed_contents_of_the_file() {
        let dir = TempDir::new().unwrap();
        let file = written(&dir, "You review code.\n");
        assert_eq!(Text::from(file.as_path()).into_string(), "You review code.");
    }

    #[test]
    fn a_path_naming_no_file_panics_with_the_path() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("absent.md");
        let panic = std::panic::catch_unwind(|| Text::from(missing.as_path())).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("absent.md"), "{message}");
    }

    #[test]
    fn from_file_reports_a_missing_file_instead_of_panicking() {
        let dir = TempDir::new().unwrap();
        let error = Text::from_file(dir.path().join("absent.md")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("absent.md"), "{error}");
    }
}
