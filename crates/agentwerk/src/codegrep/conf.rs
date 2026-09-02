//! Configures word characters, bracket pairs, case matching, and multiline matching.

/// Word characters, bracket pairs, and switches that control whitespace
/// and case behaviour during parsing and matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conf {
    /// When true, comparisons are case-insensitive.
    pub caseless: bool,
    /// When true, newlines are treated as ordinary whitespace.
    pub multiline: bool,
    /// Characters that form word tokens; runs of these are one `Word`.
    pub word_chars: Vec<char>,
    /// Pairs of opening and closing bracket characters.
    pub brackets: Vec<(char, char)>,
}

impl Conf {
    /// Defaults that treat newlines as whitespace. Word characters are
    /// `[A-Za-z0-9_]`; brackets are `()`, `[]`, `{}`.
    pub fn default_multiline() -> Self {
        Self {
            caseless: false,
            multiline: true,
            word_chars: word_chars(),
            brackets: vec![('(', ')'), ('[', ']'), ('{', '}')],
        }
    }

    /// Defaults that treat newlines as line terminators. Adds `'` and `"`
    /// to the bracket pairs so quoted strings nest as a unit.
    pub fn default_singleline() -> Self {
        let mut brackets = vec![('"', '"'), ('\'', '\'')];
        brackets.extend([('(', ')'), ('[', ']'), ('{', '}')]);
        Self {
            caseless: false,
            multiline: false,
            word_chars: word_chars(),
            brackets,
        }
    }

    /// Validate the configuration: word characters are present, bracket
    /// pairs are unique on each side, and word characters do not overlap
    /// with bracket characters.
    pub fn check(&self) -> Result<(), ConfError> {
        use std::collections::HashSet;

        if self.word_chars.is_empty() {
            return Err(ConfError("empty word characters".to_string()));
        }
        let word_set: HashSet<char> = self.word_chars.iter().copied().collect();
        let opens: HashSet<char> = self.brackets.iter().map(|(o, _)| *o).collect();
        let closes: HashSet<char> = self.brackets.iter().map(|(_, c)| *c).collect();
        if opens.len() != self.brackets.len() {
            return Err(ConfError("duplicate opening brace".to_string()));
        }
        if closes.len() != self.brackets.len() {
            return Err(ConfError("duplicate closing brace".to_string()));
        }
        let braces: HashSet<char> = opens.union(&closes).copied().collect();
        let mut conflicts: Vec<char> = word_set.intersection(&braces).copied().collect();
        if !conflicts.is_empty() {
            conflicts.sort_unstable();
            return Err(ConfError(format!(
                "word characters overlap braces: {conflicts:?}"
            )));
        }
        Ok(())
    }
}

/// A configuration that fails `Conf::check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfError(pub String);

impl std::fmt::Display for ConfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfError {}

fn word_chars() -> Vec<char> {
    let mut chars = Vec::with_capacity(63);
    chars.push('_');
    chars.extend('A'..='Z');
    chars.extend('a'..='z');
    chars.extend('0'..='9');
    chars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_default_uses_alphanumeric_word_chars_and_underscore() {
        let conf = Conf::default_multiline();
        assert!(conf.word_chars.contains(&'a'));
        assert!(conf.word_chars.contains(&'Z'));
        assert!(conf.word_chars.contains(&'0'));
        assert!(conf.word_chars.contains(&'_'));
        assert!(!conf.word_chars.contains(&'-'));
        assert!(!conf.word_chars.contains(&' '));
    }

    #[test]
    fn multiline_default_uses_standard_brackets() {
        let conf = Conf::default_multiline();
        assert_eq!(conf.brackets, vec![('(', ')'), ('[', ']'), ('{', '}')]);
    }

    #[test]
    fn multiline_default_is_case_sensitive() {
        let conf = Conf::default_multiline();
        assert!(!conf.caseless);
    }

    #[test]
    fn multiline_default_sets_multiline_flag() {
        assert!(Conf::default_multiline().multiline);
    }

    #[test]
    fn singleline_default_unsets_multiline_flag() {
        assert!(!Conf::default_singleline().multiline);
    }

    #[test]
    fn singleline_default_adds_double_and_single_quotes_to_brackets() {
        let conf = Conf::default_singleline();
        assert_eq!(
            conf.brackets,
            vec![('"', '"'), ('\'', '\''), ('(', ')'), ('[', ']'), ('{', '}')],
        );
    }

    #[test]
    fn check_accepts_a_valid_default_configuration() {
        assert!(Conf::default_multiline().check().is_ok());
        assert!(Conf::default_singleline().check().is_ok());
    }

    #[test]
    fn check_rejects_empty_word_chars() {
        let mut conf = Conf::default_multiline();
        conf.word_chars = vec![];
        assert!(conf.check().is_err());
    }

    #[test]
    fn check_rejects_duplicate_open_bracket() {
        let mut conf = Conf::default_multiline();
        conf.brackets = vec![('(', ')'), ('(', ']')];
        assert!(conf.check().is_err());
    }

    #[test]
    fn check_rejects_duplicate_close_bracket() {
        let mut conf = Conf::default_multiline();
        conf.brackets = vec![('(', ')'), ('[', ')')];
        assert!(conf.check().is_err());
    }

    #[test]
    fn check_rejects_word_char_that_is_also_a_bracket() {
        let mut conf = Conf::default_multiline();
        conf.brackets = vec![('a', 'b')];
        assert!(conf.check().is_err());
    }
}
