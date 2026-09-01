//! AQL, the one string syntax a selection is written in, and the [`Query`] it
//! parses into: over tasks by default, over recorded events as
//! `Query<Event>`.
//!
//! The tokenizer, the parser, and the condition tree are shared. A field set
//! implements [`QueryField`], which is all that separates the two grammars,
//! and [`Queryable`] names the field set a record is selected by.

use std::borrow::{Borrow, Cow};
use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use super::tasks::{now_millis, numeric_id, Status, Task};
use crate::event::Event;

/// A record a query selects, and the field set AQL names it by.
///
/// Implemented for [`Task`] and [`Event`]. Private, so the field sets and
/// everything the parser builds stay inside this module.
trait Queryable: fmt::Debug + Clone + Send + Sync + 'static {
    type Field: QueryField<Record = Self>;
}

impl Queryable for Task {
    type Field = TaskField;
}

impl Queryable for Event {
    type Field = EventField;
}

/// A condition a record is tested against.
///
/// An AQL string, a [`Query`], and closures all implement this trait, so every
/// method that selects records accepts any of them. The blanket impl for
/// `Fn(&R) -> bool` keeps closures working unchanged.
///
/// ```no_run
/// use agentwerk::{Event, Query, Task, Werk};
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let werk = Werk::new();
/// werk.find_tasks("research");
/// werk.find_tasks(Query::new("label = research AND agent = research-1")?);
/// werk.find_tasks(|t: &Task| t.get_label() == Some("research"));
/// werk.find_events("tool_call_failed");
/// werk.find_events(|e: &Event| e.get_name().ends_with("_failed"));
/// # Ok(())
/// # }
/// ```
#[allow(private_bounds)]
pub trait Matcher<R: Queryable> {
    /// Compile into a [`Query`]. A closure becomes a condition of its own, so
    /// the Werk holds one kind of filter however the caller wrote it.
    fn into_query(self) -> Query<R>;
}

impl<R: Queryable, F: Fn(&R) -> bool + Send + Sync + 'static> Matcher<R> for F {
    fn into_query(self) -> Query<R> {
        Query(Compiled::test(self))
    }
}

/// Panics on a string that does not parse. Use [`Query::new`] for one built at
/// run time.
impl<R: Queryable> Matcher<R> for &str {
    fn into_query(self) -> Query<R> {
        Query::from(self)
    }
}

impl<R: Queryable> Matcher<R> for String {
    fn into_query(self) -> Query<R> {
        Query::from(self)
    }
}

impl<R: Queryable> Matcher<R> for Query<R> {
    fn into_query(self) -> Query<R> {
        self
    }
}

/// Selects records by field values, compiled from AQL, the agentwerk query
/// syntax.
///
/// `Query` selects tasks and `Query<Event>` selects recorded events, over
/// the same syntax and a different field set.
///
/// A string says the same query wherever a matcher is taken. Compile it here
/// when the same filter runs over a large Werk or a long log, or when a
/// string built at run time should answer with an error rather than a panic.
#[allow(private_bounds)]
#[derive(Debug, Clone)]
pub struct Query<R: Queryable = Task>(Compiled<R::Field>);

#[allow(private_bounds)]
impl<R: Queryable> Query<R> {
    /// Compile an AQL string over the record's fields.
    ///
    /// ```
    /// use agentwerk::{Event, Query, Task};
    ///
    /// # fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// Query::<Task>::new("status = finished AND label IN (scan, report)")?;
    /// Query::<Task>::new("task ~ \"retry budget\" AND agent IS EMPTY")?;
    /// Query::<Task>::new("t-3")?;
    /// Query::<Task>::new("status = finished ORDER BY finished DESC")?;
    /// Query::<Task>::new("finished IS EMPTY ORDER BY created")?;
    /// Query::<Task>::new("failed > -1h")?;
    /// Query::<Task>::new("created >= 2026-08-24 AND created < 2026-08-25")?;
    /// Query::<Event>::new("event = tool_call_failed")?;
    /// Query::<Event>::new("event IN (request_failed, request_retried) AND agent = scout-1")?;
    /// Query::<Event>::new("task = t-3 ORDER BY created DESC")?;
    /// Query::<Event>::new("payload ~ timeout AND created > -1h")?;
    /// # Ok(())
    /// # }
    /// # run().unwrap();
    /// ```
    pub fn new(query: &str) -> Result<Self, QueryError> {
        Compiled::new(query).map(Query)
    }

    pub(crate) fn matches(&self, record: &R) -> bool {
        self.0.matches(record)
    }

    /// Every record, in the order the field set defaults to.
    pub(crate) fn all() -> Query<R> {
        Query(Compiled::all())
    }

    /// Keeps this query's `ORDER BY`.
    pub(crate) fn and(self, other: Query<R>) -> Query<R> {
        Query(self.0.and(other.0))
    }

    /// What lets a reader stop at the first match when no order is named.
    pub(crate) fn is_ordered(&self) -> bool {
        self.0.is_ordered()
    }

    /// Takes both the borrowed records a store hands out and the owned ones a
    /// log read produces, so neither caller copies to be sorted.
    pub(crate) fn sort<T: Borrow<R>>(&self, records: &mut [T]) {
        self.0.sort(records);
    }
}

impl Query<Task> {
    /// Also `status = <status>`, unless the query names a status of its own. A
    /// closure names none, so it always takes the default.
    pub(crate) fn default_status(self, status: Status) -> Query {
        match self.0.mentions(TaskField::Status) {
            true => self,
            false => self.and_status(status),
        }
    }

    /// Also `status = <status>`, whatever the query already says.
    pub(crate) fn and_status(self, status: Status) -> Query {
        let term = Compiled::term(TaskField::Status, Match::Is(status.to_string()));
        self.and(Query(term))
    }

    /// Also `result IS NOT EMPTY`: finishing without one is not a result.
    pub(crate) fn and_result(self) -> Query {
        self.and(Query(Compiled::term(TaskField::Result, Match::NotEmpty)))
    }
}

/// Parses the string as AQL, and panics on one that does not parse: a query
/// literal that does not compile is a mistake in the calling code, the way a
/// tool schema document the compiler refuses is. Use [`Query::new`] for a
/// string built at run time.
impl<R: Queryable> From<&str> for Query<R> {
    fn from(query: &str) -> Self {
        Query::new(query).unwrap_or_else(|error| panic!("invalid query {query:?}: {error}"))
    }
}

impl<R: Queryable> From<String> for Query<R> {
    fn from(query: String) -> Self {
        Query::from(query.as_str())
    }
}

/// One node of a parsed query.
#[derive(Debug, Clone)]
enum Condition<F: QueryField> {
    All(Vec<Condition<F>>),
    Any(Vec<Condition<F>>),
    Not(Box<Condition<F>>),
    Term(F, Match),
    Test(Predicate<F::Record>),
}

impl<F: QueryField> Condition<F> {
    fn matches(&self, record: &F::Record) -> bool {
        match self {
            Condition::All(terms) => terms.iter().all(|t| t.matches(record)),
            Condition::Any(terms) => terms.iter().any(|t| t.matches(record)),
            Condition::Not(term) => !term.matches(record),
            Condition::Term(field, matcher) => matcher.test(field.of(record).as_deref()),
            Condition::Test(check) => (check.0)(record),
        }
    }

    /// A closure names no field, so it answers `false`. That is what makes
    /// `default_status` apply to one.
    fn mentions(&self, field: F) -> bool {
        match self {
            Condition::All(terms) | Condition::Any(terms) => {
                terms.iter().any(|t| t.mentions(field))
            }
            Condition::Not(term) => term.mentions(field),
            Condition::Term(named, _) => *named == field,
            Condition::Test(_) => false,
        }
    }
}

/// A caller's closure as one condition. Its own type so `Condition` keeps its
/// derives: `Arc<dyn Fn>` carries no `Debug`, and a derived `Clone` would
/// demand one of the record.
struct Predicate<R>(Arc<dyn Fn(&R) -> bool + Send + Sync>);

impl<R> Clone for Predicate<R> {
    fn clone(&self) -> Self {
        Predicate(Arc::clone(&self.0))
    }
}

impl<R> fmt::Debug for Predicate<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<predicate>")
    }
}

/// A compiled query over one field set. [`Query`] is its one named face,
/// and everything a query does with a parsed condition it does here.
#[derive(Debug, Clone)]
struct Compiled<F: QueryField> {
    root: Condition<F>,
    /// What `ORDER BY` named, or `None` for the order the field set defaults to.
    order: Option<Sort<F>>,
}

impl<F: QueryField> Compiled<F> {
    fn new(query: &str) -> Result<Self, QueryError> {
        let (root, order) = parse_query(query)?;
        Ok(Self { root, order })
    }

    /// Every record, which AQL cannot spell beyond a bare `ORDER BY`.
    fn all() -> Self {
        Self::rooted(Condition::All(Vec::new()))
    }

    fn test(check: impl Fn(&F::Record) -> bool + Send + Sync + 'static) -> Self {
        Self::rooted(Condition::Test(Predicate(Arc::new(check))))
    }

    fn term(field: F, matcher: Match) -> Self {
        Self::rooted(Condition::Term(field, matcher))
    }

    fn rooted(root: Condition<F>) -> Self {
        Self { root, order: None }
    }

    /// Both conditions. The order is this query's, since `other` is the term a
    /// caller never wrote and so never ordered by.
    fn and(self, other: Self) -> Self {
        Self {
            root: Condition::All(vec![self.root, other.root]),
            order: self.order.or(other.order),
        }
    }

    fn matches(&self, record: &F::Record) -> bool {
        self.root.matches(record)
    }

    fn mentions(&self, field: F) -> bool {
        self.root.mentions(field)
    }

    fn is_ordered(&self) -> bool {
        self.order.is_some()
    }

    /// Takes both the borrowed records a store hands out and the owned ones a
    /// log read produces, so neither caller copies to be sorted.
    fn sort<T: Borrow<F::Record>>(&self, records: &mut [T]) {
        match &self.order {
            Some(order) => {
                records.sort_by(|left, right| order.compare(left.borrow(), right.borrow()))
            }
            None => F::sort_unordered(records),
        }
    }
}

/// One set of fields AQL names, which is all that separates the task grammar
/// from the event one. The tokenizer, the parser, [`Condition`], and
/// [`Compiled`] are shared.
trait QueryField: Copy + PartialEq + fmt::Debug + Sized + 'static {
    /// What the fields are read off. Bounded so `Condition` keeps its derives
    /// with a closure over it.
    type Record: fmt::Debug + Clone;

    /// Every spelling, in the order an unknown field is answered with.
    const FIELDS: &'static [(&'static str, Self)];

    /// What the record holds for this field, or `None` where it holds nothing.
    fn of(self, record: &Self::Record) -> Option<Cow<'_, str>>;

    fn kind(self) -> Kind;

    /// Whether the field is one a record can be missing, and so one `IS EMPTY`
    /// reads.
    fn is_optional(self) -> bool;

    /// A lone word, read as the thing this field set names most often.
    fn shorthand(word: String) -> Condition<Self>;

    /// The field a lone quoted word names, which is how a label carrying spaces
    /// is written.
    fn label() -> Self;

    /// What breaks a tie in `ORDER BY`, so one query answers one list.
    fn tie_break(record: &Self::Record) -> (u64, u32);

    /// How records lie when no `ORDER BY` names an order. A log already holds
    /// one; a store that is a map does not.
    fn sort_unordered<T: Borrow<Self::Record>>(records: &mut [T]) {
        let _ = records;
    }

    /// The value in the one spelling the record stores, rejecting a spelling
    /// no value of this field takes. Left as written unless a field overrides.
    fn canonical(self, value: String) -> Result<String, QueryError> {
        Ok(value)
    }

    /// A quoted value, which fields read like an unquoted one unless quoting
    /// deliberately escapes their canonical spelling.
    fn literal(self, value: String) -> Result<String, QueryError> {
        self.canonical(value)
    }

    /// A time by the millisecond `of` wrote, everything else as text unless
    /// the field set says otherwise.
    fn compare(self, left: &str, right: &str) -> Ordering {
        match self.kind() {
            Kind::Time => millis(left).cmp(&millis(right)),
            _ => left.cmp(right),
        }
    }

    fn named(name: &str) -> Option<Self> {
        Self::FIELDS
            .iter()
            .find(|(spelling, _)| *spelling == name)
            .map(|(_, field)| *field)
    }

    fn name(self) -> &'static str {
        Self::FIELDS
            .iter()
            .find(|(_, field)| *field == self)
            .map(|(spelling, _)| *spelling)
            .expect("every field has a spelling")
    }

    /// Every spelling as one list, for the message an unknown field answers
    /// with.
    fn spellings() -> String {
        Self::FIELDS
            .iter()
            .map(|(spelling, _)| *spelling)
            .collect::<Vec<&str>>()
            .join(", ")
    }

    fn allows(self, matcher: &Match) -> bool {
        match matcher {
            Match::Contains(_) | Match::Omits(_) => self.kind() == Kind::Text,
            Match::Empty | Match::NotEmpty => self.is_optional(),
            Match::After(_) | Match::NotBefore(_) | Match::Before(_) | Match::NotAfter(_) => {
                self.kind() == Kind::Time
            }
            _ => self.kind() == Kind::Value,
        }
    }

    /// The operators this field takes, for the message a rejected one answers
    /// with.
    fn operators(self) -> &'static str {
        match (self.kind(), self.is_optional()) {
            (Kind::Text, true) => "~, !~, IS EMPTY, IS NOT EMPTY",
            (Kind::Text, false) => "~ and !~",
            (Kind::Time, true) => ">, >=, <, <=, IS EMPTY, IS NOT EMPTY, and ORDER BY",
            (Kind::Time, false) => ">, >=, <, <=, and ORDER BY",
            (Kind::Value, true) => "=, !=, IN, NOT IN, IS EMPTY, IS NOT EMPTY",
            (Kind::Value, false) => "=, !=, IN, NOT IN",
        }
    }
}

/// A task field AQL names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskField {
    Id,
    Label,
    Status,
    Pending,
    Cancelled,
    Agent,
    Parent,
    Task,
    Result,
    Errors,
    Created,
    Started,
    Finished,
    Failed,
}

/// What a field holds, which decides the operators it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A value `=` compares.
    Value,
    /// Free text `~` searches.
    Text,
    /// A moment in milliseconds, which `>` compares and `ORDER BY` sorts.
    Time,
}

impl QueryField for TaskField {
    type Record = Task;

    const FIELDS: &'static [(&'static str, TaskField)] = &[
        ("id", TaskField::Id),
        ("label", TaskField::Label),
        ("status", TaskField::Status),
        ("pending", TaskField::Pending),
        ("cancelled", TaskField::Cancelled),
        ("agent", TaskField::Agent),
        ("parent", TaskField::Parent),
        ("task", TaskField::Task),
        ("result", TaskField::Result),
        ("errors", TaskField::Errors),
        ("created", TaskField::Created),
        ("started", TaskField::Started),
        ("finished", TaskField::Finished),
        ("failed", TaskField::Failed),
    ];

    fn of(self, task: &Task) -> Option<Cow<'_, str>> {
        match self {
            TaskField::Id => Some(Cow::Borrowed(task.id.as_str())),
            TaskField::Label => task.label.as_deref().map(Cow::Borrowed),
            TaskField::Status => Some(Cow::Owned(task.status.to_string())),
            TaskField::Pending => Some(Cow::Borrowed(bool_text(task.is_pending()))),
            TaskField::Cancelled => Some(Cow::Borrowed(bool_text(task.is_cancelled()))),
            TaskField::Agent => task.assignee.as_deref().map(Cow::Borrowed),
            TaskField::Parent => task.parent.as_deref().map(Cow::Borrowed),
            TaskField::Task => Some(as_text(&task.task)),
            TaskField::Result => task.result.as_ref().map(as_text),
            // The serialized events, so `~` reaches both the kind
            // (`"event":"tool_call_failed"`) and the message.
            TaskField::Errors => (!task.errors.is_empty())
                .then(|| Cow::Owned(serde_json::to_string(&task.errors).unwrap_or_default())),
            TaskField::Created => Some(Cow::Owned(task.created_at.to_string())),
            TaskField::Started => task.started_at.map(millis_text),
            TaskField::Finished => task.finished_at.map(millis_text),
            TaskField::Failed => task.failed_at.map(millis_text),
        }
    }

    fn is_optional(self) -> bool {
        matches!(
            self,
            TaskField::Label
                | TaskField::Agent
                | TaskField::Parent
                | TaskField::Result
                | TaskField::Errors
                | TaskField::Started
                | TaskField::Finished
                | TaskField::Failed
        )
    }

    fn kind(self) -> Kind {
        match self {
            TaskField::Task | TaskField::Result | TaskField::Errors => Kind::Text,
            TaskField::Created | TaskField::Started | TaskField::Finished | TaskField::Failed => {
                Kind::Time
            }
            _ => Kind::Value,
        }
    }

    /// The task it names by ID where it is spelled like one, and the label
    /// otherwise.
    fn shorthand(word: String) -> Condition<TaskField> {
        match is_task_id(&word) {
            true => Condition::Term(TaskField::Id, Match::Is(word)),
            false => Condition::Term(TaskField::Label, Match::Is(word)),
        }
    }

    fn label() -> TaskField {
        TaskField::Label
    }

    fn tie_break(task: &Task) -> (u64, u32) {
        (task.created_at, numeric_id(&task.id))
    }

    fn sort_unordered<T: Borrow<Task>>(tasks: &mut [T]) {
        tasks.sort_by_key(|t| Self::tie_break(t.borrow()));
    }

    /// A status in the one spelling `Status::Display` writes.
    fn canonical(self, value: String) -> Result<String, QueryError> {
        if self != TaskField::Status {
            return Ok(value);
        }
        for status in STATUSES {
            let spelling = status.to_string();
            if value.eq_ignore_ascii_case(&spelling) {
                return Ok(spelling);
            }
        }
        Err(QueryError::StatusUnrecognized { value })
    }

    /// `id` by its number, so t-2 comes before t-10, and `status`
    /// along the lifecycle.
    fn compare(self, left: &str, right: &str) -> Ordering {
        match self {
            TaskField::Id => numeric_id(left).cmp(&numeric_id(right)),
            TaskField::Status => status_rank(left).cmp(&status_rank(right)),
            TaskField::Created | TaskField::Started | TaskField::Finished | TaskField::Failed => {
                millis(left).cmp(&millis(right))
            }
            _ => left.cmp(right),
        }
    }
}

/// An event field AQL names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventField {
    Event,
    Agent,
    Task,
    Label,
    Created,
    Payload,
}

impl QueryField for EventField {
    type Record = Event;

    const FIELDS: &'static [(&'static str, EventField)] = &[
        ("event", EventField::Event),
        ("agent", EventField::Agent),
        ("task", EventField::Task),
        ("label", EventField::Label),
        ("created", EventField::Created),
        ("payload", EventField::Payload),
    ];

    fn of(self, event: &Event) -> Option<Cow<'_, str>> {
        match self {
            EventField::Event => Some(Cow::Borrowed(&event.name)),
            EventField::Agent => carried(&event.agent_id),
            EventField::Task => carried(&event.task_id),
            EventField::Label => event.label.as_deref().map(Cow::Borrowed),
            EventField::Created => Some(Cow::Owned(event.created_at.to_string())),
            EventField::Payload => serde_json::to_string(&serde_json::json!({
                "name": event.name,
                "data": event.data,
            }))
            .ok()
            .map(Cow::Owned),
        }
    }

    fn is_optional(self) -> bool {
        matches!(
            self,
            EventField::Agent | EventField::Task | EventField::Label
        )
    }

    fn kind(self) -> Kind {
        match self {
            EventField::Payload => Kind::Text,
            EventField::Created => Kind::Time,
            _ => Kind::Value,
        }
    }

    /// The event it names where the word is one, the task where it is spelled
    /// like an ID, and the label otherwise.
    fn shorthand(word: String) -> Condition<EventField> {
        if is_task_id(&word) {
            return Condition::Term(EventField::Task, Match::Is(word));
        }
        match event_named(&word) {
            Some(name) => Condition::Term(EventField::Event, Match::Is(name.to_string())),
            None => Condition::Term(EventField::Label, Match::Is(word)),
        }
    }

    fn label() -> EventField {
        EventField::Label
    }

    /// Events arrive in the order they were logged, which is what a tie keeps.
    fn tie_break(event: &Event) -> (u64, u32) {
        (event.created_at, 0)
    }

    /// A built-in event in the one snake_case spelling the log writes.
    /// Application event names are matched exactly.
    fn canonical(self, value: String) -> Result<String, QueryError> {
        if self != EventField::Event {
            return Ok(value);
        }
        Ok(event_named(&value).map_or(value, str::to_string))
    }

    fn literal(self, value: String) -> Result<String, QueryError> {
        Ok(value)
    }
}

fn is_task_id(word: &str) -> bool {
    numeric_id(word) != u32::MAX && word.starts_with("t-")
}

/// An empty string is a field the event does not carry.
fn carried(value: &str) -> Option<Cow<'_, str>> {
    (!value.is_empty()).then_some(Cow::Borrowed(value))
}

/// A built-in event in its canonical snake_case spelling.
fn event_named(value: &str) -> Option<&'static str> {
    Event::BUILTIN_NAMES
        .iter()
        .find(|name| **name == value)
        .copied()
}

/// A JSON value as the text a query compares against, matching what the
/// task tool's own search has always done with a structured task.
fn as_text(value: &serde_json::Value) -> Cow<'_, str> {
    match value {
        serde_json::Value::String(text) => Cow::Borrowed(text.as_str()),
        other => Cow::Owned(other.to_string()),
    }
}

/// The one key an `ORDER BY` clause names.
#[derive(Debug, Clone, Copy)]
struct Sort<F: QueryField> {
    field: F,
    descending: bool,
}

impl<F: QueryField> Sort<F> {
    fn compare(&self, left: &F::Record, right: &F::Record) -> Ordering {
        let placed = match (self.field.of(left), self.field.of(right)) {
            (Some(l), Some(r)) => self.field.compare(&l, &r),
            // A record the field is absent from has no value to place, so it
            // sorts last whichever way the rest is ordered.
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        let placed = match self.descending {
            true => placed.reverse(),
            false => placed,
        };
        // Ties keep the record order, so one query always answers one list.
        placed.then_with(|| F::tie_break(left).cmp(&F::tie_break(right)))
    }
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

fn bool_text(value: bool) -> &'static str {
    match value {
        true => "true",
        false => "false",
    }
}

/// A time back from the text `of` wrote it as, the way `id` reads its own
/// number back.
fn millis(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

/// The moment a comparison names, in one of three spellings: milliseconds since
/// the epoch, a `YYYY-MM-DD` date, or an offset back from now like `-30m`.
///
/// An offset is resolved here rather than at match time, so one compiled query
/// answers one set however long it is held.
fn time_value<F: QueryField>(field: F, value: &str) -> Result<u64, QueryError> {
    let resolved = match value.strip_prefix('-') {
        Some(offset) => ago(offset),
        None => match value.chars().all(|c| c.is_ascii_digit()) {
            true => value.parse().ok(),
            false => date_millis(value),
        },
    };
    resolved.ok_or_else(|| QueryError::TimeMalformed {
        field: field.name(),
        value: value.to_string(),
    })
}

/// `30m`, `2h`, `7d`, or `1w` back from now.
fn ago(offset: &str) -> Option<u64> {
    const UNITS: [(char, u64); 4] = [
        ('m', 60_000),
        ('h', 3_600_000),
        ('d', 86_400_000),
        ('w', 604_800_000),
    ];
    let (unit, span) = UNITS.iter().find(|(unit, _)| offset.ends_with(*unit))?;
    let count: u64 = offset.trim_end_matches(*unit).parse().ok()?;
    Some(now_millis().saturating_sub(count.saturating_mul(*span)))
}

/// Midnight UTC on a `YYYY-MM-DD` date, by the days-from-civil algorithm that
/// `prompts::format_current_date` runs the other way. Dates before 1970 are
/// rejected: no task and no event carries one.
fn date_millis(value: &str) -> Option<u64> {
    let mut parts = value.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86_400_000).ok()
}

fn status_rank(value: &str) -> usize {
    STATUSES
        .iter()
        .position(|status| status.to_string() == value)
        .unwrap_or(STATUSES.len())
}

/// The test one field is put to. `Contains` and `Omits` hold their needle
/// already lowercased, since they are the two that ignore case. The four times
/// hold the moment they were resolved to, in milliseconds.
#[derive(Debug, Clone)]
enum Match {
    Is(String),
    IsNot(String),
    In(Vec<String>),
    NotIn(Vec<String>),
    Contains(String),
    Omits(String),
    After(u64),
    NotBefore(u64),
    Before(u64),
    NotAfter(u64),
    Empty,
    NotEmpty,
}

impl Match {
    fn test(&self, value: Option<&str>) -> bool {
        match (self, value) {
            (Match::Empty, value) => value.is_none(),
            (Match::NotEmpty, value) => value.is_some(),
            // A field the task does not carry fails every comparison, so
            // `label != scan` never reaches an unlabelled task. IS EMPTY does.
            (_, None) => false,
            (Match::Is(wanted), Some(value)) => value == wanted,
            (Match::IsNot(rejected), Some(value)) => value != rejected,
            (Match::In(wanted), Some(value)) => wanted.iter().any(|w| w == value),
            (Match::NotIn(rejected), Some(value)) => !rejected.iter().any(|r| r == value),
            (Match::Contains(needle), Some(value)) => value.to_lowercase().contains(needle),
            (Match::Omits(needle), Some(value)) => !value.to_lowercase().contains(needle),
            (Match::After(bound), Some(value)) => millis(value) > *bound,
            (Match::NotBefore(bound), Some(value)) => millis(value) >= *bound,
            (Match::Before(bound), Some(value)) => millis(value) < *bound,
            (Match::NotAfter(bound), Some(value)) => millis(value) <= *bound,
        }
    }
}

/// What can go wrong compiling AQL. Every message names what to write instead,
/// because a host reads it in a panic and an agent reads it as a tool error.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryError {
    /// The query carried no terms at all.
    TermsMissing,
    /// No field is named this. `known` is the field set the query was compiled
    /// against, which is the task one or the event one.
    FieldUnrecognized {
        /// Field supplied by the query.
        name: String,
        /// Valid fields for this record type.
        known: String,
    },
    /// No status is spelled this way.
    StatusUnrecognized {
        /// Unrecognized status spelling.
        value: String,
    },
    /// The value a time was compared against is in none of the three spellings.
    TimeMalformed {
        /// Time field being compared.
        field: &'static str,
        /// Value that could not be parsed.
        value: String,
    },
    /// The field does not take the operator it was given.
    OperatorNotAllowed {
        /// Field that rejected the operator.
        field: &'static str,
        /// Operators accepted by the field.
        operators: &'static str,
    },
    /// Two equalities on one single-valued field, which no record satisfies.
    FieldRepeated {
        /// Single-valued field repeated by the query.
        field: &'static str,
    },
    /// A token that cannot appear where it did.
    TokenRejected {
        /// Token that cannot appear at this position.
        token: String,
    },
    /// The query stopped in the middle of a term.
    TermUnfinished,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TermsMissing => write!(
                f,
                "A query cannot be blank. Name a label, a task ID, or a field."
            ),
            Self::FieldUnrecognized { name, known } => {
                write!(f, "No field named `{name}`. Use one of {known}.")
            }
            Self::StatusUnrecognized { value } => write!(
                f,
                "No status named `{value}`. Use one of todo, in_progress, finished, failed."
            ),
            Self::TimeMalformed { field, value } => write!(
                f,
                "`{field}` compares against a time, and `{value}` is not one. \
                 Write milliseconds since the epoch, a date like `2026-08-24`, \
                 or an offset back from now like `-30m`, `-2h`, `-7d`, `-1w`."
            ),
            Self::OperatorNotAllowed { field, operators } => {
                write!(f, "`{field}` takes {operators}.")
            }
            Self::FieldRepeated { field } => write!(
                f,
                "`{field}` holds one value per task; use `{field} IN (a, b)` to match either."
            ),
            Self::TokenRejected { token } => write!(f, "Unexpected `{token}` in the query."),
            Self::TermUnfinished => write!(f, "The query ends in the middle of a term."),
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
    After,
    NotBefore,
    Before,
    NotAfter,
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
            Token::After => ">".into(),
            Token::NotBefore => ">=".into(),
            Token::Before => "<".into(),
            Token::NotAfter => "<=".into(),
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
            '>' | '<' => {
                let closed = chars.peek() == Some(&'=');
                if closed {
                    chars.next();
                }
                tokens.push(match (c, closed) {
                    ('>', false) => Token::After,
                    ('>', true) => Token::NotBefore,
                    ('<', false) => Token::Before,
                    _ => Token::NotAfter,
                });
            }
            '!' => match chars.next() {
                Some('=') => tokens.push(Token::NotEquals),
                Some('~') => tokens.push(Token::Omits),
                _ => return Err(QueryError::TokenRejected { token: "!".into() }),
            },
            '"' => {
                let mut text = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some(c) => text.push(c),
                        None => return Err(QueryError::TermUnfinished),
                    }
                }
                tokens.push(Token::Quoted(text));
            }
            _ => {
                let mut word = String::from(c);
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() || "()=~!,<>\"".contains(next) {
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

fn parse_query<F: QueryField>(query: &str) -> Result<(Condition<F>, Option<Sort<F>>), QueryError> {
    let tokens = tokenize(query)?;
    if tokens.is_empty() {
        return Err(QueryError::TermsMissing);
    }
    let mut parser = Parser {
        tokens,
        at: 0,
        field: std::marker::PhantomData,
    };
    // A query naming nothing but an order selects every record, which is how
    // the tasks tool asks for the newest without narrowing first.
    let condition = match parser.at_order_by() {
        true => Condition::All(Vec::new()),
        false => parser.any()?,
    };
    let order = parser.order_by()?;
    match parser.peek() {
        Some(token) => Err(QueryError::TokenRejected {
            token: token.spelling(),
        }),
        None => Ok((condition, order)),
    }
}

struct Parser<F: QueryField> {
    tokens: Vec<Token>,
    at: usize,
    field: std::marker::PhantomData<F>,
}

impl<F: QueryField> Parser<F> {
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
    fn order_by(&mut self) -> Result<Option<Sort<F>>, QueryError> {
        if !self.at_order_by() {
            return Ok(None);
        }
        self.at += 2;
        let name = match self.next() {
            Some(Token::Word(word)) => word,
            Some(token) => {
                return Err(QueryError::TokenRejected {
                    token: token.spelling(),
                })
            }
            None => return Err(QueryError::TermUnfinished),
        };
        let field = F::named(&name).ok_or_else(|| QueryError::FieldUnrecognized {
            name,
            known: F::spellings(),
        })?;
        let descending = self.take_keyword("desc");
        if !descending {
            self.take_keyword("asc");
        }
        Ok(Some(Sort { field, descending }))
    }

    /// `and (OR and)*`
    fn any(&mut self) -> Result<Condition<F>, QueryError> {
        let mut terms = vec![self.all()?];
        while self.take_keyword("or") {
            terms.push(self.all()?);
        }
        Ok(one_or(Condition::Any, terms))
    }

    /// `unary (AND unary)*`, where the collected terms may not name one
    /// single-valued field twice.
    fn all(&mut self) -> Result<Condition<F>, QueryError> {
        let mut terms = vec![self.unary()?];
        while self.take_keyword("and") {
            terms.push(self.unary()?);
        }
        reject_repeated_field(&terms)?;
        Ok(one_or(Condition::All, terms))
    }

    /// `NOT unary | '(' any ')' | term`
    fn unary(&mut self) -> Result<Condition<F>, QueryError> {
        if self.take_keyword("not") {
            return Ok(Condition::Not(Box::new(self.unary()?)));
        }
        if self.peek() == Some(&Token::Open) {
            self.at += 1;
            let inner = self.any()?;
            return match self.next() {
                Some(Token::Close) => Ok(inner),
                Some(token) => Err(QueryError::TokenRejected {
                    token: token.spelling(),
                }),
                None => Err(QueryError::TermUnfinished),
            };
        }
        self.term()
    }

    /// `field operator value?`, or a lone word standing for the record it
    /// names, which each field set reads its own way.
    fn term(&mut self) -> Result<Condition<F>, QueryError> {
        let token = self.next().ok_or(QueryError::TermUnfinished)?;
        let word = match token {
            Token::Word(word) => word,
            Token::Quoted(text) => return Ok(Condition::Term(F::label(), Match::Is(text))),
            other => {
                return Err(QueryError::TokenRejected {
                    token: other.spelling(),
                })
            }
        };

        // An operator after the word makes it a field reference, so a word
        // naming no field is a mistake rather than the shorthand.
        if !self.at_operator() {
            return Ok(F::shorthand(word));
        }
        let field = F::named(&word).ok_or_else(|| QueryError::FieldUnrecognized {
            name: word,
            known: F::spellings(),
        })?;
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
            Some(
                Token::Equals
                | Token::NotEquals
                | Token::Contains
                | Token::Omits
                | Token::After
                | Token::NotBefore
                | Token::Before
                | Token::NotAfter,
            ) => true,
            Some(token) => {
                token.is_keyword("in")
                    || token.is_keyword("is")
                    || (token.is_keyword("not")
                        && self.peek_after().is_some_and(|t| t.is_keyword("in")))
            }
            None => false,
        }
    }

    fn operator(&mut self, field: F) -> Result<Match, QueryError> {
        match self.next().ok_or(QueryError::TermUnfinished)? {
            Token::Equals => Ok(Match::Is(self.value(field)?)),
            Token::NotEquals => Ok(Match::IsNot(self.value(field)?)),
            Token::Contains => Ok(Match::Contains(self.value(field)?.to_lowercase())),
            Token::Omits => Ok(Match::Omits(self.value(field)?.to_lowercase())),
            Token::After => Ok(Match::After(self.time(field)?)),
            Token::NotBefore => Ok(Match::NotBefore(self.time(field)?)),
            Token::Before => Ok(Match::Before(self.time(field)?)),
            Token::NotAfter => Ok(Match::NotAfter(self.time(field)?)),
            token if token.is_keyword("in") => Ok(Match::In(self.values(field)?)),
            token if token.is_keyword("not") => {
                if !self.take_keyword("in") {
                    return Err(QueryError::TokenRejected {
                        token: "not".into(),
                    });
                }
                Ok(Match::NotIn(self.values(field)?))
            }
            token if token.is_keyword("is") => {
                let negated = self.take_keyword("not");
                if !self.take_keyword("empty") {
                    return Err(match self.next() {
                        Some(token) => QueryError::TokenRejected {
                            token: token.spelling(),
                        },
                        None => QueryError::TermUnfinished,
                    });
                }
                Ok(if negated {
                    Match::NotEmpty
                } else {
                    Match::Empty
                })
            }
            token => Err(QueryError::TokenRejected {
                token: token.spelling(),
            }),
        }
    }

    fn value(&mut self, field: F) -> Result<String, QueryError> {
        match self.next().ok_or(QueryError::TermUnfinished)? {
            Token::Word(word) => field.canonical(word),
            Token::Quoted(text) => field.literal(text),
            token => Err(QueryError::TokenRejected {
                token: token.spelling(),
            }),
        }
    }

    /// The moment a comparison names. Read before the field is checked against
    /// the operator, so `label > x` answers that `label` takes no `>` rather
    /// than complaining about `x`.
    fn time(&mut self, field: F) -> Result<u64, QueryError> {
        match self.next().ok_or(QueryError::TermUnfinished)? {
            Token::Word(word) | Token::Quoted(word) => match field.kind() {
                Kind::Time => time_value(field, &word),
                _ => Ok(0),
            },
            token => Err(QueryError::TokenRejected {
                token: token.spelling(),
            }),
        }
    }

    /// `'(' value (',' value)* ')'`, which an empty list does not satisfy.
    fn values(&mut self, field: F) -> Result<Vec<String>, QueryError> {
        match self.next() {
            Some(Token::Open) => {}
            Some(token) => {
                return Err(QueryError::TokenRejected {
                    token: token.spelling(),
                })
            }
            None => return Err(QueryError::TermUnfinished),
        }
        let mut values = vec![self.value(field)?];
        loop {
            match self.next() {
                Some(Token::Comma) => values.push(self.value(field)?),
                Some(Token::Close) => return Ok(values),
                Some(token) => {
                    return Err(QueryError::TokenRejected {
                        token: token.spelling(),
                    })
                }
                None => return Err(QueryError::TermUnfinished),
            }
        }
    }
}

fn one_or<F: QueryField>(
    group: fn(Vec<Condition<F>>) -> Condition<F>,
    mut terms: Vec<Condition<F>>,
) -> Condition<F> {
    if terms.len() == 1 {
        return terms.pop().expect("one term");
    }
    group(terms)
}

/// Two equalities on one single-valued field match no record, so they are a
/// mistake rather than a query. `OR` is left alone: that is the shape the
/// message points at.
fn reject_repeated_field<F: QueryField>(terms: &[Condition<F>]) -> Result<(), QueryError> {
    let mut seen: Vec<F> = Vec::new();
    for term in terms {
        let Condition::Term(field, Match::Is(_)) = term else {
            continue;
        };
        if seen.contains(field) {
            return Err(QueryError::FieldRepeated {
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

    fn task(id: &str) -> Task {
        let mut task = Task::new("task");
        task.id = id.to_string();
        task
    }

    /// The fields the Werk writes as a task is worked, which `Task`'s own
    /// chainable methods do not set.
    trait Fixture {
        fn agent(self, agent: &str) -> Task;
        fn status(self, status: Status) -> Task;
        fn task(self, task: serde_json::Value) -> Task;
        fn finished(self, result: serde_json::Value) -> Task;
        fn cancelled(self) -> Task;
        fn errored(self, event: crate::event::Event) -> Task;
        fn created_at(self, millis: u64) -> Task;
        fn finished_at(self, millis: u64) -> Task;
    }

    impl Fixture for Task {
        fn agent(mut self, agent: &str) -> Task {
            self.assignee = Some(agent.to_string());
            self
        }

        fn errored(mut self, event: crate::event::Event) -> Task {
            let id = self.id.clone();
            self.errors.push(event.task_id(id).agent_id("agent"));
            self
        }

        fn created_at(mut self, millis: u64) -> Task {
            self.created_at = millis;
            self
        }

        fn finished_at(mut self, millis: u64) -> Task {
            self.status = Status::Finished;
            self.finished_at = Some(millis);
            self
        }

        fn status(mut self, status: Status) -> Task {
            self.status = status;
            self
        }

        fn task(mut self, task: serde_json::Value) -> Task {
            self.task = task;
            self
        }

        fn finished(mut self, result: serde_json::Value) -> Task {
            self.status = Status::Finished;
            self.result = Some(result);
            self
        }

        fn cancelled(mut self) -> Task {
            self.cancelled = true;
            self
        }
    }

    fn parse(query: &str) -> Query {
        Query::new(query).expect("query must parse")
    }

    fn error(query: &str) -> QueryError {
        Query::<Task>::new(query).expect_err("query must be rejected")
    }

    /// The IDs the query selects, in the order it puts them in.
    fn ordered(query: &str, tasks: &[Task]) -> Vec<String> {
        let query = parse(query);
        let mut matching: Vec<&Task> = tasks.iter().filter(|t| query.matches(t)).collect();
        query.sort(&mut matching);
        matching.into_iter().map(|t| t.id.clone()).collect()
    }

    #[test]
    fn multiple_fields_and_together() {
        let q = parse("label = scan AND agent = scanner-1");
        assert!(q.matches(&task("t-1").label("scan").agent("scanner-1")));
        assert!(!q.matches(&task("t-2").label("scan").agent("scanner-2")));
        assert!(!q.matches(&task("t-3").label("report").agent("scanner-1")));
    }

    #[test]
    fn default_status_applies_to_a_query_that_leaves_status_unset() {
        let q = parse("label = scan").default_status(Status::Finished);
        assert!(q.matches(&task("t-1").label("scan").finished(json!("ok"))));
        assert!(!q.matches(&task("t-2").label("scan")));
    }

    #[test]
    fn default_status_leaves_a_query_that_sets_status_alone() {
        let q = parse("status = todo").default_status(Status::Finished);
        assert!(q.matches(&task("t-1")));
    }

    #[test]
    fn default_status_finds_a_status_nested_in_a_group() {
        let q = parse("label = scan AND NOT (status = failed)").default_status(Status::Finished);
        assert!(q.matches(&task("t-1").label("scan")));
    }

    #[test]
    fn default_status_applies_to_a_closure_which_names_no_field() {
        let q = (|t: &Task| t.label.as_deref() == Some("scan"))
            .into_query()
            .default_status(Status::Finished);
        assert!(q.matches(&task("t-1").label("scan").finished(json!("ok"))));
        assert!(!q.matches(&task("t-2").label("scan")));
    }

    #[test]
    fn and_status_applies_to_a_query_that_sets_status_of_its_own() {
        let q = parse("status = todo").and_status(Status::Finished);
        assert!(!q.matches(&task("t-1")));
        assert!(!q.matches(&task("t-2").finished(json!("ok"))));
    }

    #[test]
    fn a_closure_selects_the_tasks_it_accepts() {
        let q = (|t: &Task| t.label.as_deref() == Some("scan")).into_query();
        assert!(q.matches(&task("t-1").label("scan")));
        assert!(!q.matches(&task("t-2").label("report")));
    }

    #[test]
    fn every_way_of_naming_a_label_selects_the_same_tasks() {
        let spellings = [
            ("Query::from", Query::from("scan")),
            ("&str", Matcher::<Task>::into_query("scan")),
            ("String", Matcher::<Task>::into_query("scan".to_string())),
        ];
        for (spelling, q) in spellings {
            assert!(q.matches(&task("t-1").label("scan")), "{spelling}");
            assert!(!q.matches(&task("t-2").label("report")), "{spelling}");
            assert!(!q.matches(&task("t-3")), "{spelling}");
        }
    }

    #[test]
    fn equals_selects_the_named_value() {
        let q = parse("agent = scanner-1");
        assert!(q.matches(&task("t-1").agent("scanner-1")));
        assert!(!q.matches(&task("t-2").agent("scanner-2")));
    }

    #[test]
    fn not_equals_excludes_the_named_value() {
        let q = parse("label != scan");
        assert!(q.matches(&task("t-1").label("report")));
        assert!(!q.matches(&task("t-2").label("scan")));
    }

    #[test]
    fn an_absent_field_fails_every_comparison() {
        let unlabelled = task("t-1");
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
        assert!(q.matches(&task("t-1").label("scan")));
        assert!(q.matches(&task("t-2").label("report")));
        assert!(!q.matches(&task("t-3").label("review")));
    }

    #[test]
    fn not_in_excludes_every_listed_value() {
        let q = parse("status NOT IN (Finished, Failed)");
        assert!(q.matches(&task("t-1").status(Status::Todo)));
        assert!(!q.matches(&task("t-2").status(Status::Failed)));
    }

    #[test]
    fn pending_selects_unfinished_uncancelled_tasks() {
        let pending = parse("pending = true");
        let not_pending = parse("pending = false");
        let tasks = [
            task("t-1"),
            task("t-2").status(Status::InProgress),
            task("t-3").status(Status::Finished),
            task("t-4").status(Status::Failed),
            task("t-5").cancelled(),
        ];

        assert_eq!(ordered("pending = true", &tasks), ["t-1", "t-2"]);
        assert_eq!(ordered("pending = false", &tasks), ["t-3", "t-4", "t-5"]);
        assert!(pending.matches(&tasks[0]));
        assert!(not_pending.matches(&tasks[4]));
    }

    #[test]
    fn cancelled_selects_only_tasks_cancelled_in_this_run() {
        let cancelled = parse("cancelled = true");
        let active = parse("cancelled = false");

        assert!(cancelled.matches(&task("t-1").cancelled()));
        assert!(!cancelled.matches(&task("t-2")));
        assert!(active.matches(&task("t-3")));
    }

    #[test]
    fn id_takes_in_like_any_other_field() {
        let q = parse("id IN (t-1, t-2)");
        assert!(q.matches(&task("t-1")));
        assert!(!q.matches(&task("t-3")));
    }

    #[test]
    fn parent_selects_the_task_a_handover_came_from() {
        let q = parse("parent = t-1");
        assert!(q.matches(&task("t-2").parent("t-1")));
        assert!(!q.matches(&task("t-3")));
    }

    #[test]
    fn an_empty_in_list_is_rejected() {
        assert!(matches!(
            error("label IN ()"),
            QueryError::TokenRejected { .. }
        ));
    }

    #[test]
    fn is_empty_matches_a_task_without_a_label() {
        let q = parse("label IS EMPTY");
        assert!(q.matches(&task("t-1")));
        assert!(!q.matches(&task("t-2").label("scan")));
    }

    #[test]
    fn is_not_empty_matches_a_task_carrying_one() {
        let q = parse("label IS NOT EMPTY");
        assert!(q.matches(&task("t-1").label("scan")));
        assert!(!q.matches(&task("t-2")));
    }

    #[test]
    fn result_is_empty_matches_a_task_carrying_no_result() {
        let q = parse("result IS EMPTY");
        assert!(q.matches(&task("t-1")));
        assert!(!q.matches(&task("t-2").finished(json!("done"))));
    }

    #[test]
    fn contains_matches_the_task_body_case_insensitively() {
        let q = parse("task ~ \"Retry Budget\"");
        assert!(q.matches(&task("t-1").task(json!("check the retry budget"))));
        assert!(!q.matches(&task("t-2").task(json!("check the upload path"))));
    }

    #[test]
    fn contains_matches_a_structured_task_by_its_json_text() {
        let q = parse("task ~ db.rs");
        assert!(q.matches(&task("t-1").task(json!({"file": "src/db.rs"}))));
    }

    #[test]
    fn omits_excludes_a_matching_task() {
        let q = parse("task !~ draft");
        assert!(q.matches(&task("t-1").task(json!("final report"))));
        assert!(!q.matches(&task("t-2").task(json!("draft report"))));
    }

    #[test]
    fn contains_matches_the_stored_result() {
        let q = parse("result ~ zip");
        assert!(q.matches(&task("t-1").finished(json!({"finding": "zip slip"}))));
        assert!(!q.matches(&task("t-2").finished(json!({"finding": "clean"}))));
    }

    fn tool_failed(message: &str) -> crate::event::Event {
        crate::event::Event::new(crate::event::Event::TOOL_CALL_FAILED).data(json!({
            "tool_name": "grep",
            "call_id": "c1",
            "kind": "execution_failed",
            "message": message,
        }))
    }

    #[test]
    fn errors_is_empty_matches_a_task_without_failures() {
        let q = parse("errors IS EMPTY");
        assert!(q.matches(&task("t-1")));
        assert!(!q.matches(&task("t-2").errored(tool_failed("boom"))));
    }

    #[test]
    fn errors_is_not_empty_matches_a_task_that_failed() {
        let q = parse("errors IS NOT EMPTY");
        assert!(q.matches(&task("t-1").errored(tool_failed("boom"))));
        assert!(!q.matches(&task("t-2")));
    }

    #[test]
    fn contains_matches_a_failure_by_kind() {
        let request_failed =
            crate::event::Event::new(crate::event::Event::REQUEST_FAILED).data(json!({
                "model": "mock",
                "kind": crate::providers::RequestErrorKind::ConnectionFailed,
                "message": "boom",
            }));
        let q = parse("errors ~ tool_call_failed");
        assert!(q.matches(&task("t-1").errored(tool_failed("boom"))));
        // A different kind is excluded: the search selects by kind, not presence.
        assert!(!q.matches(&task("t-2").errored(request_failed)));
    }

    #[test]
    fn contains_matches_a_failure_message() {
        let q = parse("errors ~ timeout");
        assert!(q.matches(&task("t-1").errored(tool_failed("connection timeout"))));
        assert!(!q.matches(&task("t-2").errored(tool_failed("no such file"))));
    }

    #[test]
    fn omits_excludes_a_matching_failure() {
        let q = parse("errors !~ timeout");
        assert!(q.matches(&task("t-1").errored(tool_failed("no such file"))));
        assert!(!q.matches(&task("t-2").errored(tool_failed("connection timeout"))));
    }

    #[test]
    fn omits_does_not_match_a_task_without_failures() {
        // An optional field the task does not carry fails every comparison,
        // so `!~` excludes a clean task rather than including it.
        let q = parse("errors !~ timeout");
        assert!(!q.matches(&task("t-1")));
        assert!(q.matches(&task("t-2").errored(tool_failed("no such file"))));
    }

    #[test]
    fn contains_searches_every_recorded_failure() {
        // A task accumulates many failures; the search reads the whole log,
        // so a needle in the second failure still matches.
        let q = parse("errors ~ timeout");
        let task = task("t-1")
            .errored(tool_failed("no such file"))
            .errored(tool_failed("connection timeout"));
        assert!(q.matches(&task));
    }

    #[test]
    fn and_binds_tighter_than_or() {
        let q = parse("label = scan AND status = todo OR label = report");
        assert!(q.matches(&task("t-1").label("scan").status(Status::Todo)));
        assert!(!q.matches(&task("t-2").label("scan").status(Status::Failed)));
        assert!(q.matches(&task("t-3").label("report").status(Status::Failed)));
    }

    #[test]
    fn parentheses_override_precedence() {
        let q = parse("(label = scan OR label = report) AND status = todo");
        assert!(q.matches(&task("t-1").label("report").status(Status::Todo)));
        assert!(!q.matches(&task("t-2").label("report").status(Status::Failed)));
    }

    #[test]
    fn not_inverts_a_group() {
        let q = parse("NOT (label = scan OR label = report)");
        assert!(q.matches(&task("t-1").label("review")));
        assert!(!q.matches(&task("t-2").label("scan")));
    }

    #[test]
    fn a_quoted_value_keeps_its_spaces() {
        let q = parse("label = \"needs review\"");
        assert!(q.matches(&task("t-1").label("needs review")));
        assert!(!q.matches(&task("t-2").label("needs")));
    }

    #[test]
    fn keywords_parse_in_any_case() {
        let q = parse("label in (scan, report) and agent is not empty order by id desc");
        assert!(q.matches(&task("t-1").label("scan").agent("scanner-1")));
        assert!(!q.matches(&task("t-2").label("scan")));
    }

    #[test]
    fn a_status_parses_in_its_canonical_spelling() {
        let in_progress = task("t-1").status(Status::InProgress);
        assert!(parse("status = in_progress").matches(&in_progress));
    }

    #[test]
    fn a_value_comparison_is_case_sensitive() {
        let q = parse("label = Scan");
        assert!(!q.matches(&task("t-1").label("scan")));
    }

    #[test]
    fn a_bare_word_selects_the_label() {
        let q = parse("scan");
        assert!(q.matches(&task("t-1").label("scan")));
        assert!(!q.matches(&task("t-2").label("report")));
    }

    #[test]
    fn a_task_id_selects_that_one_task() {
        let q = parse("t-3");
        assert!(q.matches(&task("t-3").label("scan")));
        assert!(!q.matches(&task("t-4").label("scan")));
    }

    #[test]
    fn a_bare_word_inside_a_group_selects_the_label() {
        let q = parse("(scan OR report) AND status = todo");
        assert!(q.matches(&task("t-1").label("report").status(Status::Todo)));
        assert!(!q.matches(&task("t-2").label("review").status(Status::Todo)));
    }

    #[test]
    fn a_label_named_like_an_id_needs_the_field() {
        let odd = task("t-1").label("t-3");
        assert!(!parse("t-3").matches(&odd));
        assert!(parse("label = t-3").matches(&odd));
    }

    #[test]
    fn a_quoted_word_alone_selects_a_label_carrying_spaces() {
        let q = parse("\"needs review\"");
        assert!(q.matches(&task("t-1").label("needs review")));
    }

    #[test]
    fn order_by_sorts_ascending_by_default() {
        let tasks = [
            task("t-1").agent("scanner-2"),
            task("t-2").agent("scanner-1"),
        ];
        assert_eq!(ordered("ORDER BY agent", &tasks), ["t-2", "t-1"]);
    }

    #[test]
    fn a_filter_and_an_order_compose() {
        let tasks = [
            task("t-1").label("scan"),
            task("t-2").label("scan"),
            task("t-3").label("report"),
        ];
        assert_eq!(
            ordered("label = scan ORDER BY id DESC", &tasks),
            ["t-2", "t-1"]
        );
    }

    #[test]
    fn a_query_that_is_only_an_order_by_matches_every_task() {
        let q = parse("ORDER BY id");
        assert!(q.matches(&task("t-1")));
        assert!(q.matches(&task("t-2").label("scan")));
    }

    #[test]
    fn order_by_status_follows_the_lifecycle() {
        let tasks = [
            task("t-1").status(Status::Failed),
            task("t-2").status(Status::Todo),
            task("t-3").status(Status::Finished),
        ];
        assert_eq!(ordered("ORDER BY status", &tasks), ["t-2", "t-3", "t-1"]);
    }

    #[test]
    fn order_by_id_sorts_numerically_not_as_text() {
        let tasks = [task("t-10"), task("t-2")];
        assert_eq!(ordered("ORDER BY id", &tasks), ["t-2", "t-10"]);
    }

    #[test]
    fn a_task_missing_the_sort_field_sorts_last_in_both_directions() {
        let tasks = [task("t-1"), task("t-2").agent("scanner-1")];
        assert_eq!(ordered("ORDER BY agent", &tasks), ["t-2", "t-1"]);
        assert_eq!(ordered("ORDER BY agent DESC", &tasks), ["t-2", "t-1"]);
    }

    #[test]
    fn equal_sort_values_keep_creation_order() {
        // The IDs disagree with the creation times, so the assertion holds
        // only while creation order is what breaks the tie.
        let tasks = [
            task("t-1").label("scan").created_at(2),
            task("t-2").label("scan").created_at(1),
        ];
        assert_eq!(ordered("ORDER BY label", &tasks), ["t-2", "t-1"]);
    }

    #[test]
    fn a_query_without_an_order_by_answers_in_creation_order() {
        let tasks = [
            task("t-1").label("scan").created_at(2),
            task("t-2").label("scan").created_at(1),
        ];
        assert_eq!(ordered("label = scan", &tasks), ["t-2", "t-1"]);
    }

    #[test]
    fn order_by_asc_is_the_default_spelled_out() {
        let tasks = [task("t-2"), task("t-1")];
        assert_eq!(
            ordered("ORDER BY id ASC", &tasks),
            ordered("ORDER BY id", &tasks)
        );
        assert_eq!(ordered("ORDER BY id ASC", &tasks), ["t-1", "t-2"]);
    }

    #[test]
    fn order_by_finished_answers_the_most_recent_first() {
        let tasks = [
            task("t-1").finished_at(100),
            task("t-2").finished_at(300),
            task("t-3").finished_at(200),
        ];
        assert_eq!(
            ordered("ORDER BY finished DESC", &tasks),
            ["t-2", "t-3", "t-1"]
        );
    }

    #[test]
    fn order_by_finished_sorts_numerically_not_as_text() {
        let tasks = [task("t-1").finished_at(1000), task("t-2").finished_at(900)];
        assert_eq!(ordered("ORDER BY finished", &tasks), ["t-2", "t-1"]);
    }

    #[test]
    fn a_task_still_running_sorts_last_by_finished() {
        let tasks = [task("t-1"), task("t-2").finished_at(100)];
        assert_eq!(ordered("ORDER BY finished DESC", &tasks), ["t-2", "t-1"]);
    }

    #[test]
    fn finished_is_empty_matches_a_task_that_has_not_finished() {
        let q = parse("finished IS EMPTY");
        assert!(q.matches(&task("t-1")));
        assert!(!q.matches(&task("t-2").finished_at(100)));
    }

    #[test]
    fn a_time_field_takes_no_equality() {
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
                operators: ">, >=, <, <=, and ORDER BY",
            }
        );
    }

    #[test]
    fn after_selects_the_tasks_finished_since_the_moment() {
        let q = parse("finished > 200");
        assert!(q.matches(&task("t-1").finished_at(300)));
        assert!(!q.matches(&task("t-2").finished_at(200)));
        assert!(!q.matches(&task("t-3").finished_at(100)));
    }

    #[test]
    fn not_before_includes_the_moment_itself() {
        let q = parse("finished >= 200");
        assert!(q.matches(&task("t-1").finished_at(200)));
        assert!(!q.matches(&task("t-2").finished_at(199)));
    }

    #[test]
    fn before_selects_the_tasks_finished_up_to_the_moment() {
        let q = parse("finished < 200");
        assert!(q.matches(&task("t-1").finished_at(100)));
        assert!(!q.matches(&task("t-2").finished_at(200)));
    }

    #[test]
    fn not_after_includes_the_moment_itself() {
        let q = parse("finished <= 200");
        assert!(q.matches(&task("t-1").finished_at(200)));
        assert!(!q.matches(&task("t-2").finished_at(201)));
    }

    #[test]
    fn two_comparisons_on_one_time_are_a_window() {
        // `reject_repeated_field` names one field twice a mistake, and this is
        // the shape that must survive it.
        let q = parse("finished >= 200 AND finished < 400");
        assert!(q.matches(&task("t-1").finished_at(300)));
        assert!(!q.matches(&task("t-2").finished_at(400)));
        assert!(!q.matches(&task("t-3").finished_at(100)));
    }

    #[test]
    fn an_offset_selects_what_happened_inside_it() {
        let now = crate::agents::tasks::now_millis();
        let q = parse("created > -1h");
        assert!(q.matches(&task("t-1").created_at(now)));
        assert!(!q.matches(&task("t-2").created_at(now - 7_200_000)));
    }

    #[test]
    fn every_offset_unit_reaches_further_back() {
        let now = crate::agents::tasks::now_millis();
        let a_day_ago = task("t-1").created_at(now - 86_400_000);
        assert!(!parse("created > -1h").matches(&a_day_ago));
        assert!(!parse("created > -30m").matches(&a_day_ago));
        assert!(parse("created > -2d").matches(&a_day_ago));
        assert!(parse("created > -1w").matches(&a_day_ago));
    }

    #[test]
    fn a_date_is_read_as_midnight_utc() {
        // 2026-08-24T00:00:00Z, the moment the date names.
        let q = parse("created >= 2026-08-24");
        assert!(q.matches(&task("t-1").created_at(1_787_529_600_000)));
        assert!(!q.matches(&task("t-2").created_at(1_787_529_599_999)));
    }

    #[test]
    fn a_date_may_be_quoted() {
        let q = parse("created >= \"2026-08-24\"");
        assert!(q.matches(&task("t-1").created_at(1_787_529_600_000)));
    }

    #[test]
    fn an_absent_time_fails_every_comparison() {
        // The same rule an absent label follows: `IS EMPTY` is what reads it.
        let running = task("t-1");
        assert!(!parse("finished > 0").matches(&running));
        assert!(!parse("finished < 999").matches(&running));
        assert!(parse("finished IS EMPTY").matches(&running));
    }

    #[test]
    fn a_time_that_is_in_no_spelling_names_the_three() {
        let message = error("created > yesterday").to_string();
        assert!(message.contains("yesterday"), "{message}");
        assert!(message.contains("2026-08-24"), "{message}");
        assert!(message.contains("-30m"), "{message}");
    }

    #[test]
    fn an_offset_in_no_unit_is_rejected() {
        assert!(matches!(
            error("created > -5y"),
            QueryError::TimeMalformed { .. }
        ));
    }

    #[test]
    fn a_comparison_on_a_field_that_is_not_a_time_lists_the_operators() {
        let message = error("label > scan").to_string();
        assert!(message.contains("label"), "{message}");
        assert!(message.contains("IN"), "{message}");
    }

    #[test]
    fn a_comparison_needs_no_spaces_around_it() {
        let q = parse("finished>=200");
        assert!(q.matches(&task("t-1").finished_at(200)));
    }

    #[test]
    fn an_unknown_sort_field_lists_the_fields_that_exist() {
        let message = error("ORDER BY assignee").to_string();
        assert!(message.contains("assignee"), "{message}");
        assert!(message.contains("agent"), "{message}");
    }

    #[test]
    fn an_order_by_without_a_field_is_rejected() {
        assert_eq!(error("scan ORDER BY"), QueryError::TermUnfinished);
    }

    #[test]
    fn an_order_without_by_is_rejected() {
        assert!(matches!(
            error("ORDER id"),
            QueryError::TokenRejected { .. }
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
        assert!(q.matches(&task("t-1").label("scan")));
        assert!(q.matches(&task("t-2").label("report")));
    }

    #[test]
    fn an_unknown_field_lists_the_fields_that_exist() {
        let message = error("assignee = alice").to_string();
        assert!(message.contains("assignee"), "{message}");
        assert!(message.contains("agent"), "{message}");
    }

    #[test]
    fn key_is_not_an_alias_for_id() {
        assert!(matches!(
            error("key = t-1"),
            QueryError::FieldUnrecognized { name, .. } if name == "key"
        ));
    }

    #[test]
    fn an_unknown_status_lists_the_four() {
        let message = error("status = Started").to_string();
        assert!(message.contains("in_progress"), "{message}");
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
    fn is_empty_on_a_field_every_task_carries_is_rejected() {
        assert_eq!(
            error("id IS EMPTY"),
            QueryError::OperatorNotAllowed {
                field: "id",
                operators: "=, !=, IN, NOT IN",
            }
        );
    }

    #[test]
    fn an_unclosed_group_is_rejected() {
        assert_eq!(error("(label = scan"), QueryError::TermUnfinished);
    }

    #[test]
    fn an_unterminated_quote_is_rejected() {
        assert_eq!(error("label = \"needs review"), QueryError::TermUnfinished);
    }

    #[test]
    fn a_lone_bang_is_rejected() {
        assert!(matches!(
            error("label ! scan"),
            QueryError::TokenRejected { .. }
        ));
    }

    #[test]
    fn a_blank_query_is_rejected() {
        assert_eq!(error("   "), QueryError::TermsMissing);
    }

    #[test]
    #[should_panic(expected = "invalid query")]
    fn a_malformed_query_string_panics_naming_the_input() {
        Matcher::<Task>::into_query("label = ");
    }
}

/// The same grammar over the event field set.
#[cfg(test)]
mod event_tests {
    use super::*;

    fn event(event: Event) -> Event {
        event.task_id("t-1").agent_id("scout-1")
    }

    fn tool_failed(message: &str) -> Event {
        Event::new(Event::TOOL_CALL_FAILED).data(serde_json::json!({
            "tool_name": "grep",
            "call_id": "c1",
            "kind": "execution_failed",
            "message": message,
        }))
    }

    /// Pins the stamp, which `Event::new` would set to now.
    fn at(created_at: u64, value: Event) -> Event {
        Event {
            created_at,
            ..event(value)
        }
    }

    fn parse(query: &str) -> Query<Event> {
        Query::new(query).expect("query must parse")
    }

    fn error(query: &str) -> QueryError {
        Query::<Event>::new(query).expect_err("query must be rejected")
    }

    /// The events the query selects, in the order it puts them in.
    fn ordered(query: &str, events: &[Event]) -> Vec<u64> {
        let query = parse(query);
        let mut matching: Vec<Event> = events
            .iter()
            .filter(|e| query.matches(e))
            .cloned()
            .collect();
        query.sort(&mut matching);
        matching.iter().map(|e| e.created_at).collect()
    }

    #[test]
    fn an_event_is_selected_by_its_name() {
        let q = parse("event = tool_call_failed");
        assert!(q.matches(&event(tool_failed("boom"))));
        assert!(!q.matches(&event(Event::new(Event::TURN_STARTED))));
    }

    #[test]
    fn a_builtin_event_name_uses_only_its_canonical_spelling() {
        let failed = event(tool_failed("boom"));
        assert!(parse("event = tool_call_failed").matches(&failed));
        assert!(!parse("event = ToolCallFailed").matches(&failed));
    }

    #[test]
    fn an_event_takes_in_like_any_other_field() {
        let q = parse("event IN (request_failed, tool_call_failed)");
        assert!(q.matches(&event(tool_failed("boom"))));
        assert!(!q.matches(&event(Event::new(Event::TURN_STARTED))));
    }

    #[test]
    fn an_application_event_name_is_never_reinterpreted_as_a_builtin() {
        let event = Event::new("TaskFinished");
        assert!(parse(r#"event = "TaskFinished""#).matches(&event));
        assert!(parse("event = TaskFinished").matches(&event));
        assert!(!parse("event = task_finished").matches(&event));
    }

    #[test]
    fn the_task_field_selects_the_events_of_one_task() {
        let q = parse("task = t-1");
        assert!(q.matches(&event(Event::new(Event::TURN_STARTED))));
        assert!(!q.matches(
            &Event::new(Event::TURN_STARTED)
                .task_id("t-2")
                .agent_id("scout-1")
        ));
    }

    #[test]
    fn an_event_no_task_owns_reads_as_empty() {
        // `RunStarted` and `RunFinished` carry no task ID.
        let run = Event::new(Event::RUN_STARTED);
        assert!(parse("task IS EMPTY").matches(&run));
        assert!(parse("agent IS EMPTY").matches(&run));
        assert!(!parse("task IS NOT EMPTY").matches(&run));
    }

    #[test]
    fn the_agent_field_selects_who_emitted_it() {
        let q = parse("agent = scout-1");
        assert!(q.matches(&event(Event::new(Event::TURN_STARTED))));
        assert!(!q.matches(
            &Event::new(Event::TURN_STARTED)
                .task_id("t-1")
                .agent_id("sniper-1")
        ));
    }

    #[test]
    fn the_label_field_selects_the_pool_the_task_belongs_to() {
        let labelled = Event {
            label: Some("scan".into()),
            ..Event::new(Event::TURN_STARTED)
                .task_id("t-1")
                .agent_id("scout-1")
        };
        assert!(parse("label = scan").matches(&labelled));
        assert!(parse("label IS EMPTY").matches(&event(Event::new(Event::TURN_STARTED))));
    }

    #[test]
    fn payload_searches_the_message_the_kind_carries() {
        let q = parse("payload ~ timeout");
        assert!(q.matches(&event(tool_failed("connection timeout"))));
        assert!(!q.matches(&event(tool_failed("no such file"))));
    }

    #[test]
    fn payload_reaches_the_name_as_well_as_the_body() {
        assert!(parse("payload ~ tool_call_failed").matches(&event(tool_failed("boom"))));
    }

    #[test]
    fn a_lone_word_naming_an_event_selects_it() {
        let q = parse("tool_call_failed");
        assert!(q.matches(&event(tool_failed("boom"))));
        assert!(!q.matches(&event(Event::new(Event::TURN_STARTED))));
    }

    #[test]
    fn a_lone_word_naming_no_event_selects_the_label() {
        let labelled = Event {
            label: Some("scan".into()),
            ..Event::new(Event::TURN_STARTED)
                .task_id("t-1")
                .agent_id("scout-1")
        };
        let q = parse("scan");
        assert!(q.matches(&labelled));
        assert!(!q.matches(&event(Event::new(Event::TURN_STARTED))));
    }

    #[test]
    fn a_lone_task_id_selects_that_task_s_events() {
        let q = parse("t-1");
        assert!(q.matches(&event(Event::new(Event::TURN_STARTED))));
        assert!(!q.matches(
            &Event::new(Event::TURN_STARTED)
                .task_id("t-2")
                .agent_id("scout-1")
        ));
    }

    #[test]
    fn created_takes_the_same_comparisons_tasks_take() {
        let q = parse("created > 200");
        assert!(q.matches(&at(300, Event::new(Event::TURN_STARTED))));
        assert!(!q.matches(&at(100, Event::new(Event::TURN_STARTED))));
    }

    #[test]
    fn terms_combine_the_way_they_do_over_tasks() {
        let q = parse("agent = scout-1 AND (tool_call_failed OR turn_started)");
        assert!(q.matches(&event(Event::new(Event::TURN_STARTED))));
        assert!(!q.matches(&event(Event::new(Event::RUN_STARTED))));
    }

    #[test]
    fn order_by_created_desc_answers_the_newest_first() {
        let events = [
            at(100, Event::new(Event::TURN_STARTED)),
            at(300, Event::new(Event::TURN_STARTED)),
            at(200, Event::new(Event::TURN_STARTED)),
        ];
        assert_eq!(
            ordered("turn_started ORDER BY created DESC", &events),
            [300, 200, 100]
        );
    }

    #[test]
    fn a_query_without_an_order_keeps_the_order_the_log_holds() {
        let events = [
            at(300, Event::new(Event::TURN_STARTED)),
            at(100, Event::new(Event::TURN_STARTED)),
        ];
        assert_eq!(ordered("turn_started", &events), [300, 100]);
    }

    #[test]
    fn an_unknown_field_lists_the_event_fields() {
        let message = error("status = finished").to_string();
        assert!(message.contains("status"), "{message}");
        assert!(message.contains("payload"), "{message}");
        // The task fields are a different set, and the message says so.
        assert!(!message.contains("parent"), "{message}");
    }

    #[test]
    fn an_operator_the_field_rejects_lists_the_ones_it_takes() {
        let message = error("event ~ tool_call_failed").to_string();
        assert!(message.contains("event"), "{message}");
        assert!(message.contains("IN"), "{message}");
    }

    #[test]
    fn a_closure_selects_the_events_it_accepts() {
        let q = (|e: &Event| e.get_name() == Event::TOOL_CALL_FAILED).into_query();
        assert!(q.matches(&event(tool_failed("boom"))));
        assert!(!q.matches(&event(Event::new(Event::TURN_STARTED))));
    }

    #[test]
    fn a_string_says_the_same_query_a_compiled_one_does() {
        let q = Matcher::<Event>::into_query("tool_call_failed");
        assert!(q.matches(&event(tool_failed("boom"))));
        assert!(!q.matches(&event(Event::new(Event::TURN_STARTED))));
    }

    #[test]
    #[should_panic(expected = "invalid query")]
    fn a_malformed_query_string_panics_naming_the_input() {
        Matcher::<Event>::into_query("event = ");
    }
}
