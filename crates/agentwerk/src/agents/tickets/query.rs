//! A ticket matcher that selects tickets by field values, and AQL, the string
//! syntax that parses into one.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;

use super::ticket::{Status, Ticket};

/// A condition a ticket is tested against.
///
/// An AQL string, [`Query`], and closures all implement this trait, so every
/// method that selects tickets accepts any of them. The blanket impl for
/// `Fn(&Ticket) -> bool` keeps closures working unchanged.
///
/// ```no_run
/// use agentwerk::{Query, TicketQueue};
///
/// let tickets = TicketQueue::new();
/// tickets.find_tickets("research");
/// tickets.find_tickets("label = research AND status != Failed");
/// tickets.find_tickets(Query::labeled("research").agent("research-1"));
/// tickets.find_tickets(|t: &agentwerk::Ticket| t.has_label("research"));
/// ```
pub trait TicketMatcher: Send + Sync {
    fn matches(&self, ticket: &Ticket) -> bool;

    /// Whether the matcher constrains status itself. A closure cannot be asked,
    /// so it answers `false` and `TicketQueue::find_results` adds its
    /// `Finished` default.
    fn names_status(&self) -> bool {
        false
    }

    /// Order what the matcher selected. A closure names no order, so it keeps
    /// creation order, and so does a query without `ORDER BY`.
    fn sort(&self, tickets: &mut [&Ticket]) {
        sort_by_creation(tickets);
    }
}

fn sort_by_creation(tickets: &mut [&Ticket]) {
    tickets.sort_by_key(|t| (t.created_at, super::numeric_id(&t.key)));
}

impl<F: Fn(&Ticket) -> bool + Send + Sync> TicketMatcher for F {
    fn matches(&self, ticket: &Ticket) -> bool {
        self(ticket)
    }
}

/// Parses the string as AQL on every ticket it is handed, and panics on one
/// that does not parse. Pass a [`Query`] when the same filter runs over a
/// large queue.
impl TicketMatcher for &str {
    fn matches(&self, ticket: &Ticket) -> bool {
        Query::from(*self).matches(ticket)
    }

    fn names_status(&self) -> bool {
        Query::from(*self).names_status()
    }

    fn sort(&self, tickets: &mut [&Ticket]) {
        Query::from(*self).sort(tickets)
    }
}

impl TicketMatcher for String {
    fn matches(&self, ticket: &Ticket) -> bool {
        Query::from(self.as_str()).matches(ticket)
    }

    fn names_status(&self) -> bool {
        Query::from(self.as_str()).names_status()
    }

    fn sort(&self, tickets: &mut [&Ticket]) {
        Query::from(self.as_str()).sort(tickets)
    }
}

/// Selects tickets by field values, either built up field by field or parsed
/// from AQL, the agentwerk query syntax.
///
/// Every field a builder sets must match (AND). An empty query matches every
/// ticket. [`Query::parse`] accepts the same selection as a string, plus `OR`,
/// `NOT`, the operators the builders have no method for, and `ORDER BY`, which
/// only a string says.
///
/// ```no_run
/// use agentwerk::Query;
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let built = Query::labeled("scan").agent("scanner-1");
/// let parsed = Query::parse("label = scan AND agent = scanner-1")?;
/// let either = Query::parse("label IN (scan, report) AND status != Failed")?;
/// let newest = Query::parse("label = scan ORDER BY finished DESC")?;
/// # let _ = (built, parsed, either, newest);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Query {
    root: Condition,
    /// What `ORDER BY` named, or `None` for creation order.
    order: Option<Sort>,
}

impl Default for Query {
    fn default() -> Self {
        Self::new()
    }
}

impl Query {
    /// Create a query matching every ticket.
    pub fn new() -> Self {
        Self {
            root: Condition::All(Vec::new()),
            order: None,
        }
    }

    /// Create a query for one label, the field most queries select on.
    pub fn labeled(label: impl Into<String>) -> Self {
        Self::new().label(label)
    }

    /// Compile an AQL string.
    ///
    /// ```
    /// use agentwerk::Query;
    ///
    /// # fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// Query::parse("status = Finished AND label IN (scan, report)")?;
    /// Query::parse("task ~ \"retry budget\" AND agent IS EMPTY")?;
    /// Query::parse("TICKET-3")?;
    /// Query::parse("status = Finished ORDER BY finished DESC")?;
    /// Query::parse("finished IS EMPTY ORDER BY created")?;
    /// # Ok(())
    /// # }
    /// # run().unwrap();
    /// ```
    pub fn parse(query: &str) -> Result<Self, QueryError> {
        let (root, order) = parse_query(query)?;
        Ok(Self { root, order })
    }

    pub fn key(self, key: impl Into<String>) -> Self {
        self.and(Field::Key, Match::Is(key.into()))
    }

    pub fn label(self, label: impl Into<String>) -> Self {
        self.and(Field::Label, Match::Is(label.into()))
    }

    pub fn status(self, status: Status) -> Self {
        self.and(Field::Status, Match::Is(status.to_string()))
    }

    pub fn agent(self, agent: impl Into<String>) -> Self {
        self.and(Field::Agent, Match::Is(agent.into()))
    }

    pub fn parent_key(self, parent_key: impl Into<String>) -> Self {
        self.and(Field::Parent, Match::Is(parent_key.into()))
    }

    fn and(mut self, field: Field, matcher: Match) -> Self {
        let term = Condition::Term(field, matcher);
        match &mut self.root {
            Condition::All(terms) => terms.push(term),
            // A parsed root is any shape, so it becomes the first of two.
            other => {
                let parsed = std::mem::replace(other, Condition::All(Vec::new()));
                self.root = Condition::All(vec![parsed, term]);
            }
        }
        self
    }
}

impl TicketMatcher for Query {
    fn matches(&self, ticket: &Ticket) -> bool {
        self.root.matches(ticket)
    }

    fn names_status(&self) -> bool {
        self.root.mentions(Field::Status)
    }

    fn sort(&self, tickets: &mut [&Ticket]) {
        match &self.order {
            Some(order) => tickets.sort_by(|left, right| order.compare(left, right)),
            None => sort_by_creation(tickets),
        }
    }
}

/// Parses the string as AQL, and panics on one that does not parse: a query
/// literal that does not compile is a mistake in the calling code, the way a
/// tool schema document the compiler refuses is. Use [`Query::parse`] for a
/// string built at run time.
impl From<&str> for Query {
    fn from(query: &str) -> Self {
        Query::parse(query).unwrap_or_else(|error| panic!("invalid query {query:?}: {error}"))
    }
}

impl From<String> for Query {
    fn from(query: String) -> Self {
        Query::from(query.as_str())
    }
}

/// One node of a parsed query.
#[derive(Debug, Clone)]
enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    Term(Field, Match),
}

impl Condition {
    fn matches(&self, ticket: &Ticket) -> bool {
        match self {
            Condition::All(terms) => terms.iter().all(|t| t.matches(ticket)),
            Condition::Any(terms) => terms.iter().any(|t| t.matches(ticket)),
            Condition::Not(term) => !term.matches(ticket),
            Condition::Term(field, matcher) => matcher.test(field.of(ticket).as_deref()),
        }
    }

    fn mentions(&self, field: Field) -> bool {
        match self {
            Condition::All(terms) | Condition::Any(terms) => {
                terms.iter().any(|t| t.mentions(field))
            }
            Condition::Not(term) => term.mentions(field),
            Condition::Term(named, _) => *named == field,
        }
    }
}

/// A ticket field AQL names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Key,
    Label,
    Status,
    Agent,
    Parent,
    Task,
    Result,
    Created,
    Started,
    Finished,
    Failed,
}

/// Every field name AQL knows, in the order the error message lists them.
const FIELDS: [(&str, Field); 11] = [
    ("key", Field::Key),
    ("label", Field::Label),
    ("status", Field::Status),
    ("agent", Field::Agent),
    ("parent", Field::Parent),
    ("task", Field::Task),
    ("result", Field::Result),
    ("created", Field::Created),
    ("started", Field::Started),
    ("finished", Field::Finished),
    ("failed", Field::Failed),
];

/// What a field holds, which decides the operators it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A value `=` compares.
    Value,
    /// Free text `~` searches.
    Text,
    /// A moment in milliseconds, which only sorts: AQL has no `>`.
    Time,
}

impl Field {
    fn named(name: &str) -> Option<Field> {
        FIELDS
            .iter()
            .find(|(spelling, _)| *spelling == name)
            .map(|(_, field)| *field)
    }

    fn name(self) -> &'static str {
        FIELDS
            .iter()
            .find(|(_, field)| *field == self)
            .map(|(spelling, _)| *spelling)
            .expect("every Field has a spelling")
    }

    /// What the ticket holds for this field, or `None` where it holds nothing.
    fn of(self, ticket: &Ticket) -> Option<Cow<'_, str>> {
        match self {
            Field::Key => Some(Cow::Borrowed(ticket.key.as_str())),
            Field::Label => ticket.label.as_deref().map(Cow::Borrowed),
            Field::Status => Some(Cow::Owned(ticket.status.to_string())),
            Field::Agent => ticket.assignee.as_deref().map(Cow::Borrowed),
            Field::Parent => ticket.parent.as_deref().map(Cow::Borrowed),
            Field::Task => Some(as_text(&ticket.task)),
            Field::Result => ticket.result.as_ref().map(as_text),
            Field::Created => Some(Cow::Owned(ticket.created_at.to_string())),
            Field::Started => ticket.started_at.map(millis_text),
            Field::Finished => ticket.finished_at.map(millis_text),
            Field::Failed => ticket.failed_at.map(millis_text),
        }
    }

    /// Whether the field is one an agent can leave unset, and so one
    /// `IS EMPTY` reads.
    fn is_optional(self) -> bool {
        matches!(
            self,
            Field::Label
                | Field::Agent
                | Field::Parent
                | Field::Result
                | Field::Started
                | Field::Finished
                | Field::Failed
        )
    }

    fn kind(self) -> Kind {
        match self {
            Field::Task | Field::Result => Kind::Text,
            Field::Created | Field::Started | Field::Finished | Field::Failed => Kind::Time,
            _ => Kind::Value,
        }
    }

    /// How two values of this field order: `key` by its number, so TICKET-2
    /// comes before TICKET-10, `status` along the lifecycle, a time by the
    /// millisecond `of` wrote, the rest as text.
    fn compare(self, left: &str, right: &str) -> Ordering {
        match self.kind() {
            Kind::Time => millis(left).cmp(&millis(right)),
            _ => match self {
                Field::Key => super::numeric_id(left).cmp(&super::numeric_id(right)),
                Field::Status => status_rank(left).cmp(&status_rank(right)),
                _ => left.cmp(right),
            },
        }
    }

    fn allows(self, matcher: &Match) -> bool {
        match matcher {
            Match::Contains(_) | Match::Omits(_) => self.kind() == Kind::Text,
            Match::Empty | Match::NotEmpty => self.is_optional(),
            _ => self.kind() == Kind::Value,
        }
    }

    /// The operators this field takes, for the message a rejected one answers
    /// with.
    fn operators(self) -> &'static str {
        match (self.kind(), self.is_optional()) {
            (Kind::Text, true) => "~, !~, IS EMPTY, IS NOT EMPTY",
            (Kind::Text, false) => "~ and !~",
            (Kind::Time, true) => "IS EMPTY, IS NOT EMPTY, and ORDER BY",
            (Kind::Time, false) => "ORDER BY",
            (Kind::Value, true) => "=, !=, IN, NOT IN, IS EMPTY, IS NOT EMPTY",
            (Kind::Value, false) => "=, !=, IN, NOT IN",
        }
    }
}

/// A JSON value as the text a query compares against, matching what the
/// ticket tool's own search has always done with a structured task.
fn as_text(value: &serde_json::Value) -> Cow<'_, str> {
    match value {
        serde_json::Value::String(text) => Cow::Borrowed(text.as_str()),
        other => Cow::Owned(other.to_string()),
    }
}

/// The one key an `ORDER BY` clause names.
#[derive(Debug, Clone, Copy)]
struct Sort {
    field: Field,
    descending: bool,
}

impl Sort {
    fn compare(&self, left: &Ticket, right: &Ticket) -> Ordering {
        let placed = match (self.field.of(left), self.field.of(right)) {
            (Some(l), Some(r)) => self.field.compare(&l, &r),
            // A ticket the field is absent from has no value to place, so it
            // sorts last whichever way the rest is ordered.
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        let placed = match self.descending {
            true => placed.reverse(),
            false => placed,
        };
        // Ties keep creation order, so one query always answers one list.
        placed.then_with(|| created(left).cmp(&created(right)))
    }
}

fn created(ticket: &Ticket) -> (u64, u32) {
    (ticket.created_at, super::numeric_id(&ticket.key))
}

/// Every status in lifecycle order, which is the order `ORDER BY status`
/// answers in and the set a written status is matched against.
const STATUSES: [Status; 4] = [
    Status::Todo,
    Status::InProgress,
    Status::Finished,
    Status::Failed,
];

fn millis_text<'a>(millis: u64) -> Cow<'a, str> {
    Cow::Owned(millis.to_string())
}

/// A time back from the text `Field::of` wrote it as, the way `key` reads its
/// own number back.
fn millis(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

fn status_rank(value: &str) -> usize {
    STATUSES
        .iter()
        .position(|status| status.to_string() == value)
        .unwrap_or(STATUSES.len())
}

/// The test one field is put to. `Contains` and `Omits` hold their needle
/// already lowercased, since they are the two that ignore case.
#[derive(Debug, Clone)]
enum Match {
    Is(String),
    IsNot(String),
    In(Vec<String>),
    NotIn(Vec<String>),
    Contains(String),
    Omits(String),
    Empty,
    NotEmpty,
}

impl Match {
    fn test(&self, value: Option<&str>) -> bool {
        match (self, value) {
            (Match::Empty, value) => value.is_none(),
            (Match::NotEmpty, value) => value.is_some(),
            // A field the ticket does not carry fails every comparison, so
            // `label != scan` never reaches an unlabelled ticket. IS EMPTY does.
            (_, None) => false,
            (Match::Is(wanted), Some(value)) => value == wanted,
            (Match::IsNot(rejected), Some(value)) => value != rejected,
            (Match::In(wanted), Some(value)) => wanted.iter().any(|w| w == value),
            (Match::NotIn(rejected), Some(value)) => !rejected.iter().any(|r| r == value),
            (Match::Contains(needle), Some(value)) => value.to_lowercase().contains(needle),
            (Match::Omits(needle), Some(value)) => !value.to_lowercase().contains(needle),
        }
    }
}

/// What can go wrong compiling AQL. Every message names what to write instead,
/// because a host reads it in a panic and an agent reads it as a tool error.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryError {
    /// The query carried no terms at all.
    Blank,
    /// No field is named this.
    UnknownField { name: String },
    /// No status is spelled this way.
    UnknownStatus { value: String },
    /// The field does not take the operator it was given.
    OperatorNotAllowed {
        field: &'static str,
        operators: &'static str,
    },
    /// Two equalities on one single-valued field, which no ticket satisfies.
    RepeatedField { field: &'static str },
    /// A token that cannot appear where it did.
    UnexpectedToken { token: String },
    /// The query stopped in the middle of a term.
    UnexpectedEnd,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blank => write!(
                f,
                "A query cannot be blank. Name a label, a ticket key, or a field."
            ),
            Self::UnknownField { name } => {
                let known: Vec<&str> = FIELDS.iter().map(|(spelling, _)| *spelling).collect();
                write!(
                    f,
                    "No field named `{name}`. Use one of {}.",
                    known.join(", ")
                )
            }
            Self::UnknownStatus { value } => write!(
                f,
                "No status named `{value}`. Use one of Todo, InProgress, Finished, Failed."
            ),
            Self::OperatorNotAllowed { field, operators } => {
                write!(f, "`{field}` takes {operators}.")
            }
            Self::RepeatedField { field } => write!(
                f,
                "`{field}` holds one value per ticket; use `{field} IN (a, b)` to match either."
            ),
            Self::UnexpectedToken { token } => write!(f, "Unexpected `{token}` in the query."),
            Self::UnexpectedEnd => write!(f, "The query ends in the middle of a term."),
        }
    }
}

impl std::error::Error for QueryError {}

/// One piece of AQL as the parser reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A bare word, which is a keyword, a field name, or a value.
    Word(String),
    /// A quoted string, which is always a value.
    Quoted(String),
    Equals,
    NotEquals,
    Contains,
    Omits,
    Open,
    Close,
    Comma,
}

impl Token {
    /// How the token reads back in an error message.
    fn spelling(&self) -> String {
        match self {
            Token::Word(word) => word.clone(),
            Token::Quoted(text) => format!("\"{text}\""),
            Token::Equals => "=".into(),
            Token::NotEquals => "!=".into(),
            Token::Contains => "~".into(),
            Token::Omits => "!~".into(),
            Token::Open => "(".into(),
            Token::Close => ")".into(),
            Token::Comma => ",".into(),
        }
    }

    fn is_keyword(&self, keyword: &str) -> bool {
        matches!(self, Token::Word(word) if word.eq_ignore_ascii_case(keyword))
    }
}

fn tokenize(query: &str) -> Result<Vec<Token>, QueryError> {
    let mut tokens = Vec::new();
    let mut chars = query.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '(' => tokens.push(Token::Open),
            ')' => tokens.push(Token::Close),
            ',' => tokens.push(Token::Comma),
            '=' => tokens.push(Token::Equals),
            '~' => tokens.push(Token::Contains),
            '!' => match chars.next() {
                Some('=') => tokens.push(Token::NotEquals),
                Some('~') => tokens.push(Token::Omits),
                _ => return Err(QueryError::UnexpectedToken { token: "!".into() }),
            },
            '"' => {
                let mut text = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some(c) => text.push(c),
                        None => return Err(QueryError::UnexpectedEnd),
                    }
                }
                tokens.push(Token::Quoted(text));
            }
            _ => {
                let mut word = String::from(c);
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() || "()=~!,\"".contains(next) {
                        break;
                    }
                    word.push(next);
                    chars.next();
                }
                tokens.push(Token::Word(word));
            }
        }
    }
    Ok(tokens)
}

fn parse_query(query: &str) -> Result<(Condition, Option<Sort>), QueryError> {
    let tokens = tokenize(query)?;
    if tokens.is_empty() {
        return Err(QueryError::Blank);
    }
    let mut parser = Parser { tokens, at: 0 };
    // A query naming nothing but an order selects every ticket, which is how
    // the tickets tool asks for the newest without narrowing first.
    let condition = match parser.at_order_by() {
        true => Condition::All(Vec::new()),
        false => parser.any()?,
    };
    let order = parser.order_by()?;
    match parser.peek() {
        Some(token) => Err(QueryError::UnexpectedToken {
            token: token.spelling(),
        }),
        None => Ok((condition, order)),
    }
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn peek_after(&self) -> Option<&Token> {
        self.tokens.get(self.at + 1)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn take_keyword(&mut self, keyword: &str) -> bool {
        let found = self.peek().is_some_and(|t| t.is_keyword(keyword));
        if found {
            self.at += 1;
        }
        found
    }

    fn at_order_by(&self) -> bool {
        self.peek().is_some_and(|t| t.is_keyword("order"))
            && self.peek_after().is_some_and(|t| t.is_keyword("by"))
    }

    /// `ORDER BY field (ASC | DESC)?`, or nothing where the query names no
    /// order.
    fn order_by(&mut self) -> Result<Option<Sort>, QueryError> {
        if !self.at_order_by() {
            return Ok(None);
        }
        self.at += 2;
        let name = match self.next() {
            Some(Token::Word(word)) => word,
            Some(token) => {
                return Err(QueryError::UnexpectedToken {
                    token: token.spelling(),
                })
            }
            None => return Err(QueryError::UnexpectedEnd),
        };
        let field = Field::named(&name).ok_or(QueryError::UnknownField { name })?;
        let descending = self.take_keyword("desc");
        if !descending {
            self.take_keyword("asc");
        }
        Ok(Some(Sort { field, descending }))
    }

    /// `and (OR and)*`
    fn any(&mut self) -> Result<Condition, QueryError> {
        let mut terms = vec![self.all()?];
        while self.take_keyword("or") {
            terms.push(self.all()?);
        }
        Ok(one_or(Condition::Any, terms))
    }

    /// `unary (AND unary)*`, where the collected terms may not name one
    /// single-valued field twice.
    fn all(&mut self) -> Result<Condition, QueryError> {
        let mut terms = vec![self.unary()?];
        while self.take_keyword("and") {
            terms.push(self.unary()?);
        }
        reject_repeated_field(&terms)?;
        Ok(one_or(Condition::All, terms))
    }

    /// `NOT unary | '(' any ')' | term`
    fn unary(&mut self) -> Result<Condition, QueryError> {
        if self.take_keyword("not") {
            return Ok(Condition::Not(Box::new(self.unary()?)));
        }
        if self.peek() == Some(&Token::Open) {
            self.at += 1;
            let inner = self.any()?;
            return match self.next() {
                Some(Token::Close) => Ok(inner),
                Some(token) => Err(QueryError::UnexpectedToken {
                    token: token.spelling(),
                }),
                None => Err(QueryError::UnexpectedEnd),
            };
        }
        self.term()
    }

    /// `field operator value?`, or a lone word standing for the ticket it
    /// names: a key where it is spelled like one, a label otherwise.
    fn term(&mut self) -> Result<Condition, QueryError> {
        let token = self.next().ok_or(QueryError::UnexpectedEnd)?;
        let word = match token {
            Token::Word(word) => word,
            Token::Quoted(text) => return Ok(Condition::Term(Field::Label, Match::Is(text))),
            other => {
                return Err(QueryError::UnexpectedToken {
                    token: other.spelling(),
                })
            }
        };

        // An operator after the word makes it a field reference, so one no
        // field is named is a mistake rather than the shorthand.
        if !self.at_operator() {
            return Ok(shorthand(word));
        }
        let field = Field::named(&word).ok_or(QueryError::UnknownField { name: word })?;
        let matcher = self.operator(field)?;
        if !field.allows(&matcher) {
            return Err(QueryError::OperatorNotAllowed {
                field: field.name(),
                operators: field.operators(),
            });
        }
        Ok(Condition::Term(field, matcher))
    }

    fn at_operator(&self) -> bool {
        match self.peek() {
            Some(Token::Equals | Token::NotEquals | Token::Contains | Token::Omits) => true,
            Some(token) => {
                token.is_keyword("in")
                    || token.is_keyword("is")
                    || (token.is_keyword("not")
                        && self.peek_after().is_some_and(|t| t.is_keyword("in")))
            }
            None => false,
        }
    }

    fn operator(&mut self, field: Field) -> Result<Match, QueryError> {
        match self.next().ok_or(QueryError::UnexpectedEnd)? {
            Token::Equals => Ok(Match::Is(self.value(field)?)),
            Token::NotEquals => Ok(Match::IsNot(self.value(field)?)),
            Token::Contains => Ok(Match::Contains(self.value(field)?.to_lowercase())),
            Token::Omits => Ok(Match::Omits(self.value(field)?.to_lowercase())),
            token if token.is_keyword("in") => Ok(Match::In(self.values(field)?)),
            token if token.is_keyword("not") => {
                if !self.take_keyword("in") {
                    return Err(QueryError::UnexpectedToken {
                        token: "not".into(),
                    });
                }
                Ok(Match::NotIn(self.values(field)?))
            }
            token if token.is_keyword("is") => {
                let negated = self.take_keyword("not");
                if !self.take_keyword("empty") {
                    return Err(match self.next() {
                        Some(token) => QueryError::UnexpectedToken {
                            token: token.spelling(),
                        },
                        None => QueryError::UnexpectedEnd,
                    });
                }
                Ok(if negated {
                    Match::NotEmpty
                } else {
                    Match::Empty
                })
            }
            token => Err(QueryError::UnexpectedToken {
                token: token.spelling(),
            }),
        }
    }

    fn value(&mut self, field: Field) -> Result<String, QueryError> {
        match self.next().ok_or(QueryError::UnexpectedEnd)? {
            Token::Word(word) => canonical(field, word),
            Token::Quoted(text) => canonical(field, text),
            token => Err(QueryError::UnexpectedToken {
                token: token.spelling(),
            }),
        }
    }

    /// `'(' value (',' value)* ')'`, which an empty list does not satisfy.
    fn values(&mut self, field: Field) -> Result<Vec<String>, QueryError> {
        match self.next() {
            Some(Token::Open) => {}
            Some(token) => {
                return Err(QueryError::UnexpectedToken {
                    token: token.spelling(),
                })
            }
            None => return Err(QueryError::UnexpectedEnd),
        }
        let mut values = vec![self.value(field)?];
        loop {
            match self.next() {
                Some(Token::Comma) => values.push(self.value(field)?),
                Some(Token::Close) => return Ok(values),
                Some(token) => {
                    return Err(QueryError::UnexpectedToken {
                        token: token.spelling(),
                    })
                }
                None => return Err(QueryError::UnexpectedEnd),
            }
        }
    }
}

/// A lone word: the ticket it names by key where it is spelled like one, and
/// the label otherwise.
fn shorthand(word: String) -> Condition {
    if super::numeric_id(&word) != u32::MAX && word.starts_with("TICKET-") {
        return Condition::Term(Field::Key, Match::Is(word));
    }
    Condition::Term(Field::Label, Match::Is(word))
}

/// A status in the one spelling `Status::Display` writes, so both the
/// `InProgress` the tool schema documents and the `in_progress` the bindings
/// use reach the same ticket. Every other field takes its value as written.
fn canonical(field: Field, value: String) -> Result<String, QueryError> {
    if field != Field::Status {
        return Ok(value);
    }
    for status in STATUSES {
        let spelling = status.to_string();
        if value.eq_ignore_ascii_case(&spelling)
            || value.eq_ignore_ascii_case(&format!("{status:?}"))
        {
            return Ok(spelling);
        }
    }
    Err(QueryError::UnknownStatus { value })
}

fn one_or(group: fn(Vec<Condition>) -> Condition, mut terms: Vec<Condition>) -> Condition {
    if terms.len() == 1 {
        return terms.pop().expect("one term");
    }
    group(terms)
}

/// Two equalities on one single-valued field match no ticket, so they are a
/// mistake rather than a query. `OR` is left alone: that is the shape the
/// message points at.
fn reject_repeated_field(terms: &[Condition]) -> Result<(), QueryError> {
    let mut seen: Vec<Field> = Vec::new();
    for term in terms {
        let Condition::Term(field, Match::Is(_)) = term else {
            continue;
        };
        if seen.contains(field) {
            return Err(QueryError::RepeatedField {
                field: field.name(),
            });
        }
        seen.push(*field);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ticket(key: &str) -> Ticket {
        let mut ticket = Ticket::new("task");
        ticket.key = key.to_string();
        ticket
    }

    /// The fields the queue writes as a ticket is worked, which `Ticket`'s own
    /// chainable methods do not set.
    trait Fixture {
        fn agent(self, agent: &str) -> Ticket;
        fn status(self, status: Status) -> Ticket;
        fn task(self, task: serde_json::Value) -> Ticket;
        fn finished(self, result: serde_json::Value) -> Ticket;
        fn created_at(self, millis: u64) -> Ticket;
        fn finished_at(self, millis: u64) -> Ticket;
    }

    impl Fixture for Ticket {
        fn agent(mut self, agent: &str) -> Ticket {
            self.assignee = Some(agent.to_string());
            self
        }

        fn created_at(mut self, millis: u64) -> Ticket {
            self.created_at = millis;
            self
        }

        fn finished_at(mut self, millis: u64) -> Ticket {
            self.status = Status::Finished;
            self.finished_at = Some(millis);
            self
        }

        fn status(mut self, status: Status) -> Ticket {
            self.status = status;
            self
        }

        fn task(mut self, task: serde_json::Value) -> Ticket {
            self.task = task;
            self
        }

        fn finished(mut self, result: serde_json::Value) -> Ticket {
            self.status = Status::Finished;
            self.result = Some(result);
            self
        }
    }

    fn parse(query: &str) -> Query {
        Query::parse(query).expect("query must parse")
    }

    fn error(query: &str) -> QueryError {
        Query::parse(query).expect_err("query must be rejected")
    }

    /// The keys the query selects, in the order it puts them in.
    fn ordered(query: &str, tickets: &[Ticket]) -> Vec<String> {
        let query = parse(query);
        let mut matching: Vec<&Ticket> = tickets.iter().filter(|t| query.matches(t)).collect();
        query.sort(&mut matching);
        matching.into_iter().map(|t| t.key.clone()).collect()
    }

    #[test]
    fn empty_query_matches_every_ticket() {
        let q = Query::new();
        assert!(q.matches(&ticket("TICKET-1").label("scan").agent("a-1")));
        assert!(q.matches(&ticket("TICKET-2")));
    }

    #[test]
    fn key_filters_by_ticket_key() {
        let q = Query::new().key("TICKET-1");
        assert!(q.matches(&ticket("TICKET-1")));
        assert!(!q.matches(&ticket("TICKET-2")));
    }

    #[test]
    fn label_filters_by_ticket_label() {
        let q = Query::new().label("scan");
        assert!(q.matches(&ticket("TICKET-1").label("scan")));
        assert!(!q.matches(&ticket("TICKET-2").label("report")));
        assert!(!q.matches(&ticket("TICKET-3")));
    }

    #[test]
    fn status_filters_by_ticket_status() {
        let q = Query::new().status(Status::Finished);
        assert!(q.matches(&ticket("TICKET-1").status(Status::Finished)));
        assert!(!q.matches(&ticket("TICKET-1").status(Status::Todo)));
    }

    #[test]
    fn agent_filters_by_assignee() {
        let q = Query::new().agent("scanner-1");
        assert!(q.matches(&ticket("TICKET-1").agent("scanner-1")));
        assert!(!q.matches(&ticket("TICKET-2").agent("scanner-2")));
        assert!(!q.matches(&ticket("TICKET-3")));
    }

    #[test]
    fn parent_key_filters_by_parent() {
        let q = Query::new().parent_key("TICKET-1");
        assert!(q.matches(&ticket("TICKET-2").parent("TICKET-1")));
        assert!(!q.matches(&ticket("TICKET-3").parent("TICKET-5")));
        assert!(!q.matches(&ticket("TICKET-4")));
    }

    #[test]
    fn multiple_fields_and_together() {
        let q = Query::new().label("scan").agent("scanner-1");
        assert!(q.matches(&ticket("TICKET-1").label("scan").agent("scanner-1")));
        assert!(!q.matches(&ticket("TICKET-2").label("scan").agent("scanner-2")));
        assert!(!q.matches(&ticket("TICKET-3").label("report").agent("scanner-1")));
    }

    #[test]
    fn names_status_is_false_for_a_query_that_leaves_it_unset() {
        assert!(!Query::labeled("scan").names_status());
        assert!(!parse("label = scan").names_status());
    }

    #[test]
    fn names_status_is_true_for_a_query_that_sets_it() {
        assert!(Query::new().status(Status::Failed).names_status());
        assert!(parse("status != Finished").names_status());
    }

    #[test]
    fn names_status_finds_a_status_nested_in_a_group() {
        assert!(parse("label = scan AND NOT (status = Failed)").names_status());
    }

    #[test]
    fn names_status_is_false_for_a_closure() {
        let matcher: &dyn TicketMatcher = &|t: &Ticket| t.has_label("scan");
        assert!(!matcher.names_status());
    }

    #[test]
    fn a_closure_selects_the_tickets_it_accepts() {
        let matcher: &dyn TicketMatcher = &|t: &Ticket| t.has_label("scan");
        assert!(matcher.matches(&ticket("TICKET-1").label("scan")));
        assert!(!matcher.matches(&ticket("TICKET-2").label("report")));
    }

    #[test]
    fn every_way_of_naming_a_label_selects_the_same_tickets() {
        let built = Query::labeled("scan");
        let converted = Query::from("scan");
        let owned = "scan".to_string();
        let matchers: [(&str, &dyn TicketMatcher); 4] = [
            ("Query::labeled", &built),
            ("Query::from", &converted),
            ("&str", &"scan"),
            ("String", &owned),
        ];
        for (spelling, matcher) in matchers {
            assert!(
                matcher.matches(&ticket("TICKET-1").label("scan")),
                "{spelling}"
            );
            assert!(
                !matcher.matches(&ticket("TICKET-2").label("report")),
                "{spelling}"
            );
            assert!(!matcher.matches(&ticket("TICKET-3")), "{spelling}");
        }
    }

    #[test]
    fn equals_selects_the_named_value() {
        let q = parse("agent = scanner-1");
        assert!(q.matches(&ticket("TICKET-1").agent("scanner-1")));
        assert!(!q.matches(&ticket("TICKET-2").agent("scanner-2")));
    }

    #[test]
    fn not_equals_excludes_the_named_value() {
        let q = parse("label != scan");
        assert!(q.matches(&ticket("TICKET-1").label("report")));
        assert!(!q.matches(&ticket("TICKET-2").label("scan")));
    }

    #[test]
    fn an_absent_field_fails_every_comparison() {
        let unlabelled = ticket("TICKET-1");
        for query in [
            "label = scan",
            "label != scan",
            "label IN (scan, report)",
            "label NOT IN (scan, report)",
        ] {
            assert!(!parse(query).matches(&unlabelled), "{query}");
        }
    }

    #[test]
    fn in_matches_any_listed_value() {
        let q = parse("label IN (scan, report)");
        assert!(q.matches(&ticket("TICKET-1").label("scan")));
        assert!(q.matches(&ticket("TICKET-2").label("report")));
        assert!(!q.matches(&ticket("TICKET-3").label("review")));
    }

    #[test]
    fn not_in_excludes_every_listed_value() {
        let q = parse("status NOT IN (Finished, Failed)");
        assert!(q.matches(&ticket("TICKET-1").status(Status::Todo)));
        assert!(!q.matches(&ticket("TICKET-2").status(Status::Failed)));
    }

    #[test]
    fn key_takes_in_like_any_other_field() {
        let q = parse("key IN (TICKET-1, TICKET-2)");
        assert!(q.matches(&ticket("TICKET-1")));
        assert!(!q.matches(&ticket("TICKET-3")));
    }

    #[test]
    fn parent_selects_the_ticket_a_handover_came_from() {
        let q = parse("parent = TICKET-1");
        assert!(q.matches(&ticket("TICKET-2").parent("TICKET-1")));
        assert!(!q.matches(&ticket("TICKET-3")));
    }

    #[test]
    fn an_empty_in_list_is_rejected() {
        assert!(matches!(
            error("label IN ()"),
            QueryError::UnexpectedToken { .. }
        ));
    }

    #[test]
    fn is_empty_matches_a_ticket_without_a_label() {
        let q = parse("label IS EMPTY");
        assert!(q.matches(&ticket("TICKET-1")));
        assert!(!q.matches(&ticket("TICKET-2").label("scan")));
    }

    #[test]
    fn is_not_empty_matches_a_ticket_carrying_one() {
        let q = parse("label IS NOT EMPTY");
        assert!(q.matches(&ticket("TICKET-1").label("scan")));
        assert!(!q.matches(&ticket("TICKET-2")));
    }

    #[test]
    fn result_is_empty_matches_a_ticket_carrying_no_result() {
        let q = parse("result IS EMPTY");
        assert!(q.matches(&ticket("TICKET-1")));
        assert!(!q.matches(&ticket("TICKET-2").finished(json!("done"))));
    }

    #[test]
    fn contains_matches_the_task_body_case_insensitively() {
        let q = parse("task ~ \"Retry Budget\"");
        assert!(q.matches(&ticket("TICKET-1").task(json!("check the retry budget"))));
        assert!(!q.matches(&ticket("TICKET-2").task(json!("check the upload path"))));
    }

    #[test]
    fn contains_matches_a_structured_task_by_its_json_text() {
        let q = parse("task ~ db.rs");
        assert!(q.matches(&ticket("TICKET-1").task(json!({"file": "src/db.rs"}))));
    }

    #[test]
    fn omits_excludes_a_matching_task() {
        let q = parse("task !~ draft");
        assert!(q.matches(&ticket("TICKET-1").task(json!("final report"))));
        assert!(!q.matches(&ticket("TICKET-2").task(json!("draft report"))));
    }

    #[test]
    fn contains_matches_the_stored_result() {
        let q = parse("result ~ zip");
        assert!(q.matches(&ticket("TICKET-1").finished(json!({"finding": "zip slip"}))));
        assert!(!q.matches(&ticket("TICKET-2").finished(json!({"finding": "clean"}))));
    }

    #[test]
    fn and_binds_tighter_than_or() {
        let q = parse("label = scan AND status = Todo OR label = report");
        assert!(q.matches(&ticket("TICKET-1").label("scan").status(Status::Todo)));
        assert!(!q.matches(&ticket("TICKET-2").label("scan").status(Status::Failed)));
        assert!(q.matches(&ticket("TICKET-3").label("report").status(Status::Failed)));
    }

    #[test]
    fn parentheses_override_precedence() {
        let q = parse("(label = scan OR label = report) AND status = Todo");
        assert!(q.matches(&ticket("TICKET-1").label("report").status(Status::Todo)));
        assert!(!q.matches(&ticket("TICKET-2").label("report").status(Status::Failed)));
    }

    #[test]
    fn not_inverts_a_group() {
        let q = parse("NOT (label = scan OR label = report)");
        assert!(q.matches(&ticket("TICKET-1").label("review")));
        assert!(!q.matches(&ticket("TICKET-2").label("scan")));
    }

    #[test]
    fn a_quoted_value_keeps_its_spaces() {
        let q = parse("label = \"needs review\"");
        assert!(q.matches(&ticket("TICKET-1").label("needs review")));
        assert!(!q.matches(&ticket("TICKET-2").label("needs")));
    }

    #[test]
    fn keywords_parse_in_any_case() {
        let q = parse("label in (scan, report) and agent is not empty order by key desc");
        assert!(q.matches(&ticket("TICKET-1").label("scan").agent("scanner-1")));
        assert!(!q.matches(&ticket("TICKET-2").label("scan")));
    }

    #[test]
    fn a_status_parses_in_both_spellings() {
        let in_progress = ticket("TICKET-1").status(Status::InProgress);
        assert!(parse("status = InProgress").matches(&in_progress));
        assert!(parse("status = in_progress").matches(&in_progress));
    }

    #[test]
    fn a_value_comparison_is_case_sensitive() {
        let q = parse("label = Scan");
        assert!(!q.matches(&ticket("TICKET-1").label("scan")));
    }

    #[test]
    fn a_bare_word_selects_the_label() {
        let q = parse("scan");
        assert!(q.matches(&ticket("TICKET-1").label("scan")));
        assert!(!q.matches(&ticket("TICKET-2").label("report")));
    }

    #[test]
    fn a_ticket_key_selects_that_one_ticket() {
        let q = parse("TICKET-3");
        assert!(q.matches(&ticket("TICKET-3").label("scan")));
        assert!(!q.matches(&ticket("TICKET-4").label("scan")));
    }

    #[test]
    fn a_bare_word_inside_a_group_selects_the_label() {
        let q = parse("(scan OR report) AND status = Todo");
        assert!(q.matches(&ticket("TICKET-1").label("report").status(Status::Todo)));
        assert!(!q.matches(&ticket("TICKET-2").label("review").status(Status::Todo)));
    }

    #[test]
    fn a_label_named_like_a_key_needs_the_field() {
        let odd = ticket("TICKET-1").label("TICKET-3");
        assert!(!parse("TICKET-3").matches(&odd));
        assert!(parse("label = TICKET-3").matches(&odd));
    }

    #[test]
    fn a_quoted_word_alone_selects_a_label_carrying_spaces() {
        let q = parse("\"needs review\"");
        assert!(q.matches(&ticket("TICKET-1").label("needs review")));
    }

    #[test]
    fn order_by_sorts_ascending_by_default() {
        let tickets = [
            ticket("TICKET-1").agent("scanner-2"),
            ticket("TICKET-2").agent("scanner-1"),
        ];
        assert_eq!(
            ordered("ORDER BY agent", &tickets),
            ["TICKET-2", "TICKET-1"]
        );
    }

    #[test]
    fn a_filter_and_an_order_compose() {
        let tickets = [
            ticket("TICKET-1").label("scan"),
            ticket("TICKET-2").label("scan"),
            ticket("TICKET-3").label("report"),
        ];
        assert_eq!(
            ordered("label = scan ORDER BY key DESC", &tickets),
            ["TICKET-2", "TICKET-1"]
        );
    }

    #[test]
    fn a_query_that_is_only_an_order_by_matches_every_ticket() {
        let q = parse("ORDER BY key");
        assert!(q.matches(&ticket("TICKET-1")));
        assert!(q.matches(&ticket("TICKET-2").label("scan")));
    }

    #[test]
    fn order_by_status_follows_the_lifecycle() {
        let tickets = [
            ticket("TICKET-1").status(Status::Failed),
            ticket("TICKET-2").status(Status::Todo),
            ticket("TICKET-3").status(Status::Finished),
        ];
        assert_eq!(
            ordered("ORDER BY status", &tickets),
            ["TICKET-2", "TICKET-3", "TICKET-1"]
        );
    }

    #[test]
    fn order_by_key_sorts_numerically_not_as_text() {
        let tickets = [ticket("TICKET-10"), ticket("TICKET-2")];
        assert_eq!(ordered("ORDER BY key", &tickets), ["TICKET-2", "TICKET-10"]);
    }

    #[test]
    fn a_ticket_missing_the_sort_field_sorts_last_in_both_directions() {
        let tickets = [ticket("TICKET-1"), ticket("TICKET-2").agent("scanner-1")];
        assert_eq!(
            ordered("ORDER BY agent", &tickets),
            ["TICKET-2", "TICKET-1"]
        );
        assert_eq!(
            ordered("ORDER BY agent DESC", &tickets),
            ["TICKET-2", "TICKET-1"]
        );
    }

    #[test]
    fn equal_sort_values_keep_creation_order() {
        // The keys disagree with the creation times, so the assertion holds
        // only while creation order is what breaks the tie.
        let tickets = [
            ticket("TICKET-1").label("scan").created_at(2),
            ticket("TICKET-2").label("scan").created_at(1),
        ];
        assert_eq!(
            ordered("ORDER BY label", &tickets),
            ["TICKET-2", "TICKET-1"]
        );
    }

    #[test]
    fn a_query_without_an_order_by_answers_in_creation_order() {
        let tickets = [
            ticket("TICKET-1").label("scan").created_at(2),
            ticket("TICKET-2").label("scan").created_at(1),
        ];
        assert_eq!(ordered("label = scan", &tickets), ["TICKET-2", "TICKET-1"]);
    }

    #[test]
    fn order_by_asc_is_the_default_spelled_out() {
        let tickets = [ticket("TICKET-2"), ticket("TICKET-1")];
        assert_eq!(
            ordered("ORDER BY key ASC", &tickets),
            ordered("ORDER BY key", &tickets)
        );
        assert_eq!(
            ordered("ORDER BY key ASC", &tickets),
            ["TICKET-1", "TICKET-2"]
        );
    }

    #[test]
    fn order_by_finished_answers_the_most_recent_first() {
        let tickets = [
            ticket("TICKET-1").finished_at(100),
            ticket("TICKET-2").finished_at(300),
            ticket("TICKET-3").finished_at(200),
        ];
        assert_eq!(
            ordered("ORDER BY finished DESC", &tickets),
            ["TICKET-2", "TICKET-3", "TICKET-1"]
        );
    }

    #[test]
    fn order_by_finished_sorts_numerically_not_as_text() {
        let tickets = [
            ticket("TICKET-1").finished_at(1000),
            ticket("TICKET-2").finished_at(900),
        ];
        assert_eq!(
            ordered("ORDER BY finished", &tickets),
            ["TICKET-2", "TICKET-1"]
        );
    }

    #[test]
    fn a_ticket_still_running_sorts_last_by_finished() {
        let tickets = [ticket("TICKET-1"), ticket("TICKET-2").finished_at(100)];
        assert_eq!(
            ordered("ORDER BY finished DESC", &tickets),
            ["TICKET-2", "TICKET-1"]
        );
    }

    #[test]
    fn finished_is_empty_matches_a_ticket_that_has_not_finished() {
        let q = parse("finished IS EMPTY");
        assert!(q.matches(&ticket("TICKET-1")));
        assert!(!q.matches(&ticket("TICKET-2").finished_at(100)));
    }

    #[test]
    fn a_time_field_takes_no_comparison() {
        let message = error("finished = 3").to_string();
        assert!(message.contains("finished"), "{message}");
        assert!(message.contains("ORDER BY"), "{message}");
    }

    #[test]
    fn created_is_never_empty() {
        assert_eq!(
            error("created IS EMPTY"),
            QueryError::OperatorNotAllowed {
                field: "created",
                operators: "ORDER BY",
            }
        );
    }

    #[test]
    fn an_unknown_sort_field_lists_the_fields_that_exist() {
        let message = error("ORDER BY assignee").to_string();
        assert!(message.contains("assignee"), "{message}");
        assert!(message.contains("agent"), "{message}");
    }

    #[test]
    fn an_order_by_without_a_field_is_rejected() {
        assert_eq!(error("scan ORDER BY"), QueryError::UnexpectedEnd);
    }

    #[test]
    fn an_order_without_by_is_rejected() {
        assert!(matches!(
            error("ORDER key"),
            QueryError::UnexpectedToken { .. }
        ));
    }

    #[test]
    fn two_equalities_on_one_label_are_rejected() {
        let message = error("label = scan AND label = report").to_string();
        assert!(message.contains("label"), "{message}");
        assert!(message.contains("IN"), "{message}");
    }

    #[test]
    fn two_equalities_on_one_label_are_allowed_under_or() {
        let q = parse("label = scan OR label = report");
        assert!(q.matches(&ticket("TICKET-1").label("scan")));
        assert!(q.matches(&ticket("TICKET-2").label("report")));
    }

    #[test]
    fn an_unknown_field_lists_the_fields_that_exist() {
        let message = error("assignee = alice").to_string();
        assert!(message.contains("assignee"), "{message}");
        assert!(message.contains("agent"), "{message}");
    }

    #[test]
    fn an_unknown_status_lists_the_four() {
        let message = error("status = Started").to_string();
        assert!(message.contains("InProgress"), "{message}");
    }

    #[test]
    fn an_operator_the_field_rejects_lists_the_ones_it_takes() {
        let message = error("task = anything").to_string();
        assert!(message.contains("task"), "{message}");
        assert!(message.contains('~'), "{message}");
    }

    #[test]
    fn a_text_operator_on_a_value_field_lists_the_ones_it_takes() {
        let message = error("label ~ scan").to_string();
        assert!(message.contains("label"), "{message}");
        assert!(message.contains("IN"), "{message}");
    }

    #[test]
    fn is_empty_on_a_field_every_ticket_carries_is_rejected() {
        assert_eq!(
            error("key IS EMPTY"),
            QueryError::OperatorNotAllowed {
                field: "key",
                operators: "=, !=, IN, NOT IN",
            }
        );
    }

    #[test]
    fn an_unclosed_group_is_rejected() {
        assert_eq!(error("(label = scan"), QueryError::UnexpectedEnd);
    }

    #[test]
    fn an_unterminated_quote_is_rejected() {
        assert_eq!(error("label = \"needs review"), QueryError::UnexpectedEnd);
    }

    #[test]
    fn a_lone_bang_is_rejected() {
        assert!(matches!(
            error("label ! scan"),
            QueryError::UnexpectedToken { .. }
        ));
    }

    #[test]
    fn a_blank_query_is_rejected() {
        assert_eq!(error("   "), QueryError::Blank);
    }

    #[test]
    #[should_panic(expected = "invalid query")]
    fn a_malformed_query_string_panics_naming_the_input() {
        let matcher: &dyn TicketMatcher = &"label = ";
        matcher.matches(&ticket("TICKET-1"));
    }

    #[test]
    fn a_parsed_query_matches_the_same_tickets_as_the_builder() {
        let built = Query::labeled("scan").agent("scanner-1");
        let parsed = parse("label = scan AND agent = scanner-1");
        for t in [
            ticket("TICKET-1").label("scan").agent("scanner-1"),
            ticket("TICKET-2").label("scan").agent("scanner-2"),
            ticket("TICKET-3").label("report"),
            ticket("TICKET-4"),
        ] {
            assert_eq!(built.matches(&t), parsed.matches(&t), "{}", t.key);
        }
    }
}
