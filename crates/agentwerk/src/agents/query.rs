//! Parses AQL into queries over tasks, events, and task-event joins.

use std::borrow::{Borrow, Cow};
use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use super::tasks::{now_millis, numeric_id, Status, Task};
use crate::event::Event;

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
/// werk.find_tasks(Query::new("task.label = research AND task.assignee = research-1")?);
/// werk.find_tasks(|t: &Task| t.get_label() == Some("research"));
/// werk.find_events("event.name = tool_call_failed");
/// werk.find_events("research AND event.name = tool_call_failed");
/// werk.find_events(|e: &Event| e.get_name().ends_with("_failed"));
/// # Ok(())
/// # }
/// ```
pub trait Matcher<R> {
    /// Compile into a [`Query`]. A closure becomes a condition of its own, so
    /// the Werk holds one kind of filter however the caller wrote it.
    fn into_query(self) -> Query;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(id: &str) -> Task {
        let mut task = Task::new(json!({"work": "scan"})).label("scan");
        task.id = id.to_string();
        task.created_at = numeric_id(id) as u64;
        task
    }

    #[test]
    fn every_supported_field_infers_its_origin_and_accepts_its_operator() {
        let cases = [
            ("task.id = t-1", Origin::Task),
            ("task.label = scan", Origin::Task),
            ("task.status = finished", Origin::Task),
            ("task.pending = true", Origin::Task),
            ("task.cancelled = false", Origin::Task),
            ("task.assignee = agent-1", Origin::Task),
            ("task.parent_id = t-1", Origin::Task),
            ("task.input ~ scan", Origin::Task),
            ("task.result ~ clean", Origin::Task),
            ("task.errors ~ timeout", Origin::Task),
            ("task.created > 0", Origin::Task),
            ("task.started > 0", Origin::Task),
            ("task.finished > 0", Origin::Task),
            ("task.failed > 0", Origin::Task),
            ("event.name = task_created", Origin::Event),
            ("event.agent_id = agent-1", Origin::Event),
            ("event.task_id = t-1", Origin::Event),
            ("event.label = scan", Origin::Event),
            ("event.created > 0", Origin::Event),
            ("event.data ~ scan", Origin::Event),
        ];

        for (source, origin) in cases {
            let query = Query::new(source).unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(query.origin(), origin, "{source}");
        }
    }

    #[test]
    fn bare_values_are_task_label_shorthand() {
        let scan = task("t-1");
        let review = task("t-2").label("needs review");

        for (source, subject) in [("scan", &scan), ("\"needs review\"", &review)] {
            let query = Query::new(source).unwrap();
            assert_eq!(query.origin(), Origin::Task, "{source}");
            assert!(query.matches_task(subject), "{source}");
        }
    }

    #[test]
    fn bare_task_ids_take_precedence_over_label_shorthand() {
        let id_match = task("t-3").label("other");
        let label_match = task("t-4").label("t-3");
        let query = Query::new("t-3").unwrap();

        assert!(query.matches_task(&id_match));
        assert!(!query.matches_task(&label_match));
    }

    #[test]
    fn order_only_queries_infer_an_origin() {
        assert_eq!(
            Query::new("ORDER BY event.created DESC").unwrap().origin(),
            Origin::Event
        );
    }

    #[test]
    fn blank_queries_are_rejected() {
        assert!(matches!(Query::new("  "), Err(QueryError::TermsMissing)));
    }

    #[test]
    fn unqualified_field_expressions_are_rejected() {
        let error = Query::new("label = scan").unwrap_err();

        assert!(matches!(&error, QueryError::FieldUnrecognized { .. }));
        assert!(error.to_string().contains("task.label"));
    }

    #[test]
    fn bare_event_names_are_task_labels_not_event_shorthand() {
        let labelled = task("t-1").label(Event::TOOL_CALL_FAILED);
        let query = Query::new("tool_call_failed").unwrap();

        assert_eq!(query.origin(), Origin::Task);
        assert!(query.matches_task(&labelled));
    }

    #[test]
    fn the_removed_result_namespace_is_rejected() {
        for field in [
            "result.task_id",
            "result.label",
            "result.status",
            "result.pending",
            "result.cancelled",
            "result.assignee",
            "result.parent_id",
            "result.input",
            "result.value",
            "result.errors",
            "result.created",
            "result.started",
            "result.finished",
            "result.failed",
        ] {
            assert!(
                matches!(
                    Query::new(&format!("{field} = value")),
                    Err(QueryError::FieldUnrecognized { .. })
                ),
                "{field}"
            );
        }
    }

    #[test]
    fn using_both_namespaces_infers_a_joined_source() {
        for source in [
            "scan AND event.name = task_finished",
            "task.label = scan AND event.label = scan",
            "task.label = scan OR event.label = scan",
            "task.label = scan AND NOT event.name = task_failed",
            "task.label = scan ORDER BY event.created",
        ] {
            assert_eq!(
                Query::new(source).unwrap().origin(),
                Origin::Joined,
                "{source}"
            );
        }
    }

    #[test]
    fn boolean_membership_presence_and_text_operators_match_tasks() {
        let scan = task("t-1");
        let mut report = task("t-2");
        report.label = Some("report".into());
        let mut unlabelled = task("t-3");
        unlabelled.label = None;

        for (source, subject, expected) in [
            ("task.label IN (scan, review)", &scan, true),
            ("task.label NOT IN (scan, review)", &report, true),
            ("NOT (task.label = report OR task.id = t-2)", &scan, true),
            ("task.assignee IS EMPTY", &scan, true),
            ("task.assignee IS NOT EMPTY", &scan, false),
            ("task.input ~ SCAN", &scan, true),
            ("task.input !~ report", &scan, true),
            ("task.label != scan", &unlabelled, false),
        ] {
            assert_eq!(
                Query::new(source).unwrap().matches_task(subject),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn time_operators_compare_milliseconds_and_accept_dates_and_offsets() {
        let mut subject = task("t-100");
        subject.created_at = 100;

        assert!(Query::new("task.created > 99")
            .unwrap()
            .matches_task(&subject));
        assert!(Query::new("task.created <= 100")
            .unwrap()
            .matches_task(&subject));
        assert!(Query::new("task.started IS EMPTY")
            .unwrap()
            .matches_task(&subject));
        Query::new("task.created >= 2026-08-24").unwrap();
        Query::new("task.finished < -1h").unwrap();
    }

    #[test]
    fn statuses_are_canonicalized_before_matching() {
        let mut subject = task("t-1");
        subject.status = Status::InProgress;

        for spelling in ["in_progress", "IN_PROGRESS", "In_Progress"] {
            assert!(Query::new(&format!("task.status = {spelling}"))
                .unwrap()
                .matches_task(&subject));
        }
    }

    #[test]
    fn custom_event_names_match_exactly() {
        let event = Event::new("ApplicationEvent");
        assert!(Query::new("event.name = ApplicationEvent")
            .unwrap()
            .matches_event(&event));
        assert!(!Query::new("event.name = applicationevent")
            .unwrap()
            .matches_event(&event));
    }

    #[test]
    fn malformed_statuses_and_times_report_their_error_kind() {
        assert!(matches!(
            Query::new("task.status = unknown"),
            Err(QueryError::StatusUnrecognized { .. })
        ));
        assert!(matches!(
            Query::new("task.created > tomorrow"),
            Err(QueryError::TimeMalformed { .. })
        ));
    }

    #[test]
    fn fields_reject_operators_for_another_value_kind() {
        for source in ["task.input = retry", "task.id ~ t-1", "task.created = 0"] {
            assert!(
                matches!(
                    Query::new(source),
                    Err(QueryError::OperatorNotAllowed { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn repeated_equalities_suggest_membership() {
        let error = Query::new("task.id = t-1 AND task.id = t-2").unwrap_err();

        assert!(matches!(&error, QueryError::FieldRepeated { .. }));
        assert!(error.to_string().contains("task.id IN (a, b)"));
    }

    #[test]
    fn trailing_tokens_are_rejected() {
        assert!(matches!(
            Query::new("task.id = t-1 trailing"),
            Err(QueryError::TokenRejected { .. })
        ));
    }

    #[test]
    fn unfinished_terms_are_rejected() {
        assert!(matches!(
            Query::new("task.id ="),
            Err(QueryError::TermUnfinished)
        ));
    }

    #[test]
    fn event_data_searches_raw_data_but_not_the_event_name() {
        let named_only = Event::new(Event::TOOL_CALL_FAILED).data(json!({"message": "boom"}));
        let named_in_data = Event::new(Event::TURN_STARTED)
            .data(json!({"kind": "tool_call_failed", "message": "boom"}));
        let data = Query::new("event.data ~ tool_call_failed").unwrap();
        let name = Query::new("event.name = tool_call_failed").unwrap();

        assert!(!data.matches_event(&named_only));
        assert!(data.matches_event(&named_in_data));
        assert!(name.matches_event(&named_only));
        assert!(!name.matches_event(&named_in_data));
    }

    #[test]
    fn task_result_searches_the_optional_raw_result() {
        let mut finished = task("t-1");
        finished.status = Status::Finished;
        finished.result = Some(json!({"finding": "clean"}));

        assert!(Query::new("task.result ~ clean")
            .unwrap()
            .matches_task(&finished));
    }

    #[test]
    fn task_ids_sort_by_their_numeric_suffix() {
        let mut tasks = vec![task("t-10"), task("t-2"), task("t-1")];
        Query::new("ORDER BY task.id")
            .unwrap()
            .sort_tasks(&mut tasks);
        assert_eq!(
            tasks
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            ["t-1", "t-2", "t-10"]
        );
    }

    #[test]
    fn statuses_sort_in_lifecycle_order() {
        let mut tasks = vec![task("t-1"), task("t-2"), task("t-3")];
        tasks[0].status = Status::Failed;
        tasks[1].status = Status::Finished;
        tasks[2].status = Status::Todo;
        Query::new("ORDER BY task.status")
            .unwrap()
            .sort_tasks(&mut tasks);
        assert_eq!(
            tasks.iter().map(|task| task.status).collect::<Vec<_>>(),
            [Status::Todo, Status::Finished, Status::Failed]
        );
    }

    #[test]
    fn missing_optional_values_sort_last_in_both_directions() {
        let mut tasks = vec![task("t-1"), task("t-2"), task("t-3")];
        tasks[0].assignee = None;
        tasks[1].assignee = Some("a".into());
        tasks[2].assignee = Some("z".into());
        Query::new("ORDER BY task.assignee DESC")
            .unwrap()
            .sort_tasks(&mut tasks);
        assert_eq!(tasks.last().unwrap().assignee, None);

        Query::new("ORDER BY task.assignee")
            .unwrap()
            .sort_tasks(&mut tasks);
        assert_eq!(tasks.last().unwrap().assignee, None);
    }

    #[test]
    fn mixed_conditions_are_evaluated_against_one_joined_pair() {
        let scan = task("t-1");
        let matching = Event::new(Event::TASK_CREATED)
            .task_id("t-1")
            .data(json!({"kind": "scan"}));
        let other = Event::new(Event::TASK_FAILED).task_id("t-1");
        let wrong_task = task("t-2").label("report");
        let query =
            Query::new("task.label = scan AND event.name = task_created AND event.data ~ scan")
                .unwrap();

        assert!(query.matches_joined(&scan, &matching));
        assert!(!query.matches_joined(&scan, &other));
        assert!(!query.matches_joined(&wrong_task, &matching));
    }

    #[test]
    fn joined_boolean_operators_evaluate_each_task_event_pair() {
        let scan = task("t-1");
        let report = task("t-2").label("report");
        let selected = Event::new("selected");
        let ordinary = Event::new("ordinary");
        let either = Query::new("task.label = scan OR event.name = selected").unwrap();
        let neither = Query::new("NOT task.label = report AND NOT event.name = selected").unwrap();

        assert!(either.matches_joined(&scan, &ordinary));
        assert!(either.matches_joined(&report, &selected));
        assert!(!either.matches_joined(&report, &ordinary));
        assert!(neither.matches_joined(&scan, &ordinary));
        assert!(!neither.matches_joined(&report, &ordinary));
        assert!(!neither.matches_joined(&scan, &selected));
    }

    #[test]
    fn joined_sorting_can_order_events_by_a_task_field() {
        let first = task("t-1");
        let second = task("t-2");
        let mut pairs = vec![
            (second.clone(), Event::new("second-a")),
            (first.clone(), Event::new("first-a")),
            (second, Event::new("second-b")),
            (first, Event::new("first-b")),
        ];
        Query::new("event.name IN (first-a, first-b, second-a, second-b) ORDER BY task.id")
            .unwrap()
            .sort_joined(&mut pairs);

        assert_eq!(
            pairs
                .iter()
                .map(|(task, event)| (task.id.as_str(), event.name.as_str()))
                .collect::<Vec<_>>(),
            [
                ("t-1", "first-a"),
                ("t-1", "first-b"),
                ("t-2", "second-a"),
                ("t-2", "second-b"),
            ]
        );
    }
}

impl<F: Fn(&Task) -> bool + Send + Sync + 'static> Matcher<Task> for F {
    fn into_query(self) -> Query {
        Query(Compiled::test(Predicate::Task(Arc::new(self))))
    }
}

impl<F: Fn(&Event) -> bool + Send + Sync + 'static> Matcher<Event> for F {
    fn into_query(self) -> Query {
        Query(Compiled::test(Predicate::Event(Arc::new(self))))
    }
}

/// Panics on a string that does not parse. Use [`Query::new`] for one built at
/// run time.
impl<R> Matcher<R> for &str {
    fn into_query(self) -> Query {
        Query::from(self)
    }
}

impl<R> Matcher<R> for String {
    fn into_query(self) -> Query {
        Query::from(self)
    }
}

impl<R> Matcher<R> for Query {
    fn into_query(self) -> Query {
        self
    }
}

/// Select records by field values expressed in AQL.
///
/// A string says the same query wherever a matcher is taken. Compile it here
/// when the same filter runs over a large Werk or a long log, or when a
/// string built at run time should answer with an error rather than a panic.
#[derive(Debug, Clone)]
pub struct Query(Compiled);

impl Query {
    /// Compile an AQL string. Task and event fields may be joined through a
    /// recorded event's task ID. A lone value matches `task.label`, except a
    /// bare `t-N`, which matches `task.id`.
    ///
    /// ```
    /// use agentwerk::Query;
    ///
    /// # fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// Query::new("research")?;
    /// Query::new("\"needs review\"")?;
    /// Query::new("task.status = finished AND task.label IN (scan, report)")?;
    /// Query::new("task.input ~ \"retry budget\" AND task.assignee IS EMPTY")?;
    /// Query::new("t-3")?;
    /// Query::new("task.label = scan ORDER BY task.finished DESC")?;
    /// Query::new("event.name = tool_call_failed")?;
    /// Query::new("event.data ~ timeout AND event.created > -1h")?;
    /// Query::new("scan AND event.name = task_finished")?;
    /// # Ok(())
    /// # }
    /// # run().unwrap();
    /// ```
    pub fn new(query: &str) -> Result<Self, QueryError> {
        Compiled::new(query).map(Query)
    }

    pub(crate) fn matches_task(&self, task: &Task) -> bool {
        self.assert_origin(Origin::Task);
        self.0.matches(View::Task(task))
    }

    pub(crate) fn matches_event(&self, event: &Event) -> bool {
        self.assert_origin(Origin::Event);
        self.0.matches(View::Event(event))
    }

    pub(crate) fn matches_joined(&self, task: &Task, event: &Event) -> bool {
        self.assert_origin(Origin::Joined);
        self.0.matches(View::Joined(task, event))
    }

    /// Every record, in its origin's default order.
    pub(crate) fn all() -> Query {
        Query(Compiled::all())
    }

    /// Keeps this query's `ORDER BY`.
    pub(crate) fn and(self, other: Query) -> Query {
        Query(self.0.and(other.0))
    }

    /// What lets a reader stop at the first match when no order is named.
    pub(crate) fn is_ordered(&self) -> bool {
        self.0.is_ordered()
    }

    /// Takes both the borrowed records a store hands out and the owned ones a
    /// log read produces, so neither caller copies to be sorted.
    pub(crate) fn sort_tasks<T: Borrow<Task>>(&self, records: &mut [T]) {
        self.assert_origin(Origin::Task);
        self.0.sort_tasks(records);
    }

    pub(crate) fn sort_events<T: Borrow<Event>>(&self, records: &mut [T]) {
        self.assert_origin(Origin::Event);
        self.0.sort_events(records);
    }

    pub(crate) fn sort_joined(&self, records: &mut [(Task, Event)]) {
        self.assert_origin(Origin::Joined);
        self.0.sort_joined(records);
    }

    pub(crate) fn task(mut self) -> Query {
        self.0.bind(Origin::Task);
        self
    }

    pub(crate) fn task_if_originless(mut self) -> Query {
        self.0.bind_if_originless(Origin::Task);
        self
    }

    pub(crate) fn event_if_originless(mut self) -> Query {
        self.0.bind_if_originless(Origin::Event);
        self
    }

    pub(crate) fn origin(&self) -> Origin {
        self.0.origin.unwrap_or(Origin::Task)
    }

    fn assert_origin(&self, expected: Origin) {
        assert!(
            self.0.origin.is_none() || self.0.origin == Some(expected),
            "query resolver must match its source"
        );
    }

    /// Also `status = <status>`, whatever the query already says.
    pub(crate) fn and_task_status(self, status: Status) -> Query {
        let term = Compiled::term(Field::TaskStatus, Match::Is(status.to_string()));
        self.and(Query(term))
    }

    pub(crate) fn default_task_status(self, status: Status) -> Query {
        match self.0.mentions(Field::TaskStatus) {
            true => self,
            false => self.and_task_status(status),
        }
    }

    pub(crate) fn and_task_result(self) -> Query {
        self.and(Query(Compiled::term(Field::TaskResult, Match::NotEmpty)))
    }
}

/// Parses the string as AQL, and panics on one that does not parse: a query
/// literal that does not compile is a mistake in the calling code, the way a
/// tool schema document the compiler refuses is. Use [`Query::new`] for a
/// string built at run time.
impl From<&str> for Query {
    fn from(query: &str) -> Self {
        Query::new(query).unwrap_or_else(|error| panic!("invalid query {query:?}: {error}"))
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
    Test(Predicate),
}

impl Condition {
    fn matches(&self, record: View<'_>) -> bool {
        match self {
            Condition::All(terms) => terms.iter().all(|t| t.matches(record)),
            Condition::Any(terms) => terms.iter().any(|t| t.matches(record)),
            Condition::Not(term) => !term.matches(record),
            Condition::Term(field, matcher) => matcher.test(record.value(*field).as_deref()),
            Condition::Test(check) => check.test(record),
        }
    }

    /// A closure names no field, so it answers `false`. That is what makes a
    /// result finder's default task status apply to one.
    fn mentions(&self, field: Field) -> bool {
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
enum Predicate {
    Task(Arc<dyn Fn(&Task) -> bool + Send + Sync>),
    Event(Arc<dyn Fn(&Event) -> bool + Send + Sync>),
}

impl Predicate {
    fn test(&self, record: View<'_>) -> bool {
        match (self, record) {
            (Predicate::Task(check), View::Task(task)) => check(task),
            (Predicate::Event(check), View::Event(event)) => check(event),
            (Predicate::Task(check), View::Joined(task, _)) => check(task),
            (Predicate::Event(check), View::Joined(_, event)) => check(event),
            _ => false,
        }
    }
}

impl Clone for Predicate {
    fn clone(&self) -> Self {
        match self {
            Predicate::Task(check) => Predicate::Task(Arc::clone(check)),
            Predicate::Event(check) => Predicate::Event(Arc::clone(check)),
        }
    }
}

impl fmt::Debug for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<predicate>")
    }
}

/// A parsed query and its inferred origin, behind [`Query`].
#[derive(Debug, Clone)]
struct Compiled {
    root: Condition,
    /// What `ORDER BY` named, or `None` for the source origin's default order.
    order: Option<Sort>,
    origin: Option<Origin>,
}

impl Compiled {
    fn new(query: &str) -> Result<Self, QueryError> {
        let (root, order, origin) = parse_query(query)?;
        Ok(Self {
            root,
            order,
            origin: Some(origin),
        })
    }

    /// Every record, which AQL cannot spell beyond a bare `ORDER BY`.
    fn all() -> Self {
        Self::rooted(Condition::All(Vec::new()))
    }

    fn test(check: Predicate) -> Self {
        Self::rooted(Condition::Test(check))
    }

    fn term(field: Field, matcher: Match) -> Self {
        Self {
            root: Condition::Term(field, matcher),
            order: None,
            origin: Some(field.origin()),
        }
    }

    fn rooted(root: Condition) -> Self {
        Self {
            root,
            order: None,
            origin: None,
        }
    }

    /// Both conditions. The order is this query's, since `other` is the term a
    /// caller never wrote and so never ordered by.
    fn and(self, other: Self) -> Self {
        let origin = merge_origins(self.origin, other.origin);
        Self {
            root: Condition::All(vec![self.root, other.root]),
            order: self.order.or(other.order),
            origin,
        }
    }

    fn matches(&self, record: View<'_>) -> bool {
        self.root.matches(record)
    }

    fn mentions(&self, field: Field) -> bool {
        self.root.mentions(field)
    }

    fn is_ordered(&self) -> bool {
        self.order.is_some()
    }

    /// Takes both the borrowed records a store hands out and the owned ones a
    /// log read produces, so neither caller copies to be sorted.
    fn sort_tasks<T: Borrow<Task>>(&self, records: &mut [T]) {
        match &self.order {
            Some(order) => records.sort_by(|left, right| {
                order.compare(View::Task(left.borrow()), View::Task(right.borrow()))
            }),
            None => records.sort_by_key(|task| {
                let task = task.borrow();
                (task.created_at, numeric_id(&task.id))
            }),
        }
    }

    fn sort_events<T: Borrow<Event>>(&self, records: &mut [T]) {
        if let Some(order) = &self.order {
            records.sort_by(|left, right| {
                order.compare(View::Event(left.borrow()), View::Event(right.borrow()))
            });
        }
    }

    fn sort_joined(&self, records: &mut [(Task, Event)]) {
        if let Some(order) = &self.order {
            records.sort_by(|(left_task, left_event), (right_task, right_event)| {
                order.compare(
                    View::Joined(left_task, left_event),
                    View::Joined(right_task, right_event),
                )
            });
        }
    }

    fn bind(&mut self, expected: Origin) {
        assert!(
            self.origin.is_none() || self.origin == Some(expected),
            "query binding must not replace its source"
        );
        self.origin = Some(expected);
    }

    fn bind_if_originless(&mut self, origin: Origin) {
        if self.origin.is_none() {
            self.origin = Some(origin);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Task,
    Event,
    Joined,
}

fn merge_origins(left: Option<Origin>, right: Option<Origin>) -> Option<Origin> {
    match (left, right) {
        (Some(left), Some(right)) if left != right => Some(Origin::Joined),
        (Some(origin), _) | (_, Some(origin)) => Some(origin),
        (None, None) => None,
    }
}

#[derive(Debug, Clone, Copy)]
enum View<'a> {
    Task(&'a Task),
    Event(&'a Event),
    Joined(&'a Task, &'a Event),
}

impl<'a> View<'a> {
    fn value(self, field: Field) -> Option<Cow<'a, str>> {
        match self {
            View::Task(task) => field.of_task(task),
            View::Event(event) => field.of_event(event),
            View::Joined(task, event) => match field.origin() {
                Origin::Task => field.of_task(task),
                Origin::Event => field.of_event(event),
                Origin::Joined => unreachable!("fields have one origin"),
            },
        }
    }

    fn tie_break(self) -> (u64, u32) {
        match self {
            View::Task(task) => (task.created_at, numeric_id(&task.id)),
            View::Event(event) => (event.created_at, 0),
            View::Joined(task, event) => (event.created_at, numeric_id(&task.id)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    TaskId,
    TaskLabel,
    TaskStatus,
    TaskPending,
    TaskCancelled,
    TaskAssignee,
    TaskParentId,
    TaskInput,
    TaskResult,
    TaskErrors,
    TaskCreated,
    TaskStarted,
    TaskFinished,
    TaskFailed,
    EventName,
    EventAgentId,
    EventTaskId,
    EventLabel,
    EventCreated,
    EventData,
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

impl Field {
    const FIELDS: &'static [(&'static str, Field)] = &[
        ("task.id", Field::TaskId),
        ("task.label", Field::TaskLabel),
        ("task.status", Field::TaskStatus),
        ("task.pending", Field::TaskPending),
        ("task.cancelled", Field::TaskCancelled),
        ("task.assignee", Field::TaskAssignee),
        ("task.parent_id", Field::TaskParentId),
        ("task.input", Field::TaskInput),
        ("task.result", Field::TaskResult),
        ("task.errors", Field::TaskErrors),
        ("task.created", Field::TaskCreated),
        ("task.started", Field::TaskStarted),
        ("task.finished", Field::TaskFinished),
        ("task.failed", Field::TaskFailed),
        ("event.name", Field::EventName),
        ("event.agent_id", Field::EventAgentId),
        ("event.task_id", Field::EventTaskId),
        ("event.label", Field::EventLabel),
        ("event.created", Field::EventCreated),
        ("event.data", Field::EventData),
    ];

    fn named(name: &str) -> Option<Self> {
        Self::FIELDS
            .iter()
            .find_map(|(spelling, field)| (*spelling == name).then_some(*field))
    }

    fn spellings() -> String {
        Self::FIELDS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn name(self) -> &'static str {
        Self::FIELDS
            .iter()
            .find_map(|(name, field)| (*field == self).then_some(*name))
            .expect("every field has one spelling")
    }

    fn origin(self) -> Origin {
        match self {
            Self::TaskId
            | Self::TaskLabel
            | Self::TaskStatus
            | Self::TaskPending
            | Self::TaskCancelled
            | Self::TaskAssignee
            | Self::TaskParentId
            | Self::TaskInput
            | Self::TaskResult
            | Self::TaskErrors
            | Self::TaskCreated
            | Self::TaskStarted
            | Self::TaskFinished
            | Self::TaskFailed => Origin::Task,
            Self::EventName
            | Self::EventAgentId
            | Self::EventTaskId
            | Self::EventLabel
            | Self::EventCreated
            | Self::EventData => Origin::Event,
        }
    }

    fn kind(self) -> Kind {
        match self {
            Self::TaskInput | Self::TaskResult | Self::TaskErrors | Self::EventData => Kind::Text,
            Self::TaskCreated
            | Self::TaskStarted
            | Self::TaskFinished
            | Self::TaskFailed
            | Self::EventCreated => Kind::Time,
            _ => Kind::Value,
        }
    }

    fn is_optional(self) -> bool {
        matches!(
            self,
            Self::TaskLabel
                | Self::TaskAssignee
                | Self::TaskParentId
                | Self::TaskResult
                | Self::TaskErrors
                | Self::TaskStarted
                | Self::TaskFinished
                | Self::TaskFailed
                | Self::EventAgentId
                | Self::EventTaskId
                | Self::EventLabel
        )
    }

    fn allows(self, matcher: &Match) -> bool {
        match matcher {
            Match::Empty | Match::NotEmpty => self.is_optional(),
            Match::Contains(_) | Match::Omits(_) => self.kind() == Kind::Text,
            Match::After(_) | Match::NotBefore(_) | Match::Before(_) | Match::NotAfter(_) => {
                self.kind() == Kind::Time
            }
            _ => self.kind() == Kind::Value,
        }
    }

    fn operators(self) -> &'static str {
        match (self.kind(), self.is_optional()) {
            (Kind::Value, false) => "=, !=, IN, or NOT IN",
            (Kind::Value, true) => "=, !=, IN, NOT IN, IS EMPTY, or IS NOT EMPTY",
            (Kind::Text, false) => "~ or !~",
            (Kind::Text, true) => "~, !~, IS EMPTY, or IS NOT EMPTY",
            (Kind::Time, false) => ">, >=, <, or <=",
            (Kind::Time, true) => ">, >=, <, <=, IS EMPTY, or IS NOT EMPTY",
        }
    }

    fn canonical(self, value: String) -> Result<String, QueryError> {
        if self == Self::TaskStatus {
            for status in STATUSES {
                let spelling = status.to_string();
                if value.eq_ignore_ascii_case(&spelling) {
                    return Ok(spelling);
                }
            }
            return Err(QueryError::StatusUnrecognized { value });
        }
        if self == Self::EventName {
            return Ok(event_named(&value).map_or(value, str::to_string));
        }
        Ok(value)
    }

    fn literal(self, value: String) -> Result<String, QueryError> {
        match self {
            Self::EventName => Ok(value),
            _ => self.canonical(value),
        }
    }

    fn compare(self, left: &str, right: &str) -> Ordering {
        match self {
            Self::TaskId => numeric_id(left).cmp(&numeric_id(right)),
            Self::TaskStatus => status_rank(left).cmp(&status_rank(right)),
            Self::TaskCreated
            | Self::TaskStarted
            | Self::TaskFinished
            | Self::TaskFailed
            | Self::EventCreated => millis(left).cmp(&millis(right)),
            _ => left.cmp(right),
        }
    }

    fn of_task(self, task: &Task) -> Option<Cow<'_, str>> {
        match self {
            Self::TaskId => Some(Cow::Borrowed(task.id.as_str())),
            Self::TaskLabel => task.label.as_deref().map(Cow::Borrowed),
            Self::TaskStatus => Some(Cow::Owned(task.status.to_string())),
            Self::TaskPending => Some(Cow::Borrowed(bool_text(task.is_pending()))),
            Self::TaskCancelled => Some(Cow::Borrowed(bool_text(task.is_cancelled()))),
            Self::TaskAssignee => task.assignee.as_deref().map(Cow::Borrowed),
            Self::TaskParentId => task.parent.as_deref().map(Cow::Borrowed),
            Self::TaskInput => Some(as_text(&task.task)),
            Self::TaskResult => task.result.as_ref().map(as_text),
            Self::TaskErrors => serialized_errors(task),
            Self::TaskCreated => Some(millis_text(task.created_at)),
            Self::TaskStarted => task.started_at.map(millis_text),
            Self::TaskFinished => task.finished_at.map(millis_text),
            Self::TaskFailed => task.failed_at.map(millis_text),
            _ => None,
        }
    }

    fn of_event(self, event: &Event) -> Option<Cow<'_, str>> {
        match self {
            Self::EventName => Some(Cow::Borrowed(&event.name)),
            Self::EventAgentId => carried(&event.agent_id),
            Self::EventTaskId => carried(&event.task_id),
            Self::EventLabel => event.label.as_deref().map(Cow::Borrowed),
            Self::EventCreated => Some(millis_text(event.created_at)),
            Self::EventData => serde_json::to_string(&event.data).ok().map(Cow::Owned),
            _ => None,
        }
    }
}

fn serialized_errors(task: &Task) -> Option<Cow<'_, str>> {
    (!task.errors.is_empty())
        .then(|| Cow::Owned(serde_json::to_string(&task.errors).unwrap_or_default()))
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
struct Sort {
    field: Field,
    descending: bool,
}

impl Sort {
    fn compare(&self, left: View<'_>, right: View<'_>) -> Ordering {
        let placed = match (left.value(self.field), right.value(self.field)) {
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
        placed.then_with(|| match (self.field.origin(), left, right) {
            (Origin::Task, View::Joined(left, _), View::Joined(right, _)) => {
                (left.created_at, numeric_id(&left.id))
                    .cmp(&(right.created_at, numeric_id(&right.id)))
            }
            (Origin::Event, View::Joined(_, left), View::Joined(_, right)) => {
                left.created_at.cmp(&right.created_at)
            }
            _ => left.tie_break().cmp(&right.tie_break()),
        })
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
fn time_value(field: Field, value: &str) -> Result<u64, QueryError> {
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
    /// No field is named this. `known` lists every task and event field.
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
                "A query cannot be blank. Name an origin-qualified field or a task ID."
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
                "`{field}` holds one value per record; use `{field} IN (a, b)` to match either."
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

fn parse_query(query: &str) -> Result<(Condition, Option<Sort>, Origin), QueryError> {
    let tokens = tokenize(query)?;
    if tokens.is_empty() {
        return Err(QueryError::TermsMissing);
    }
    let mut parser = Parser {
        tokens,
        at: 0,
        origin: None,
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
        None => Ok((
            condition,
            order,
            parser.origin.expect("a nonblank query names an origin"),
        )),
    }
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    origin: Option<Origin>,
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
                return Err(QueryError::TokenRejected {
                    token: token.spelling(),
                })
            }
            None => return Err(QueryError::TermUnfinished),
        };
        let field = self.field(name)?;
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
                Some(token) => Err(QueryError::TokenRejected {
                    token: token.spelling(),
                }),
                None => Err(QueryError::TermUnfinished),
            };
        }
        self.term()
    }

    /// `field operator value?`, a bare task ID, or a task-label shorthand.
    fn term(&mut self) -> Result<Condition, QueryError> {
        let token = self.next().ok_or(QueryError::TermUnfinished)?;
        let word = match token {
            Token::Word(word) => word,
            Token::Quoted(text) => {
                self.record_field(Field::TaskLabel);
                return Ok(Condition::Term(Field::TaskLabel, Match::Is(text)));
            }
            other => {
                return Err(QueryError::TokenRejected {
                    token: other.spelling(),
                })
            }
        };

        if !self.at_operator() {
            let field = if is_task_id(&word) {
                Field::TaskId
            } else {
                Field::TaskLabel
            };
            self.record_field(field);
            return Ok(Condition::Term(field, Match::Is(word)));
        }
        let field = self.field(word)?;
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

    fn operator(&mut self, field: Field) -> Result<Match, QueryError> {
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

    fn value(&mut self, field: Field) -> Result<String, QueryError> {
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
    fn time(&mut self, field: Field) -> Result<u64, QueryError> {
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
    fn values(&mut self, field: Field) -> Result<Vec<String>, QueryError> {
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

    fn field(&mut self, name: String) -> Result<Field, QueryError> {
        let field = Field::named(&name).ok_or_else(|| QueryError::FieldUnrecognized {
            name,
            known: Field::spellings(),
        })?;
        self.record_field(field);
        Ok(field)
    }

    fn record_field(&mut self, field: Field) {
        self.origin = merge_origins(self.origin, Some(field.origin()));
    }
}

fn one_or(group: fn(Vec<Condition>) -> Condition, mut terms: Vec<Condition>) -> Condition {
    if terms.len() == 1 {
        return terms.pop().expect("one term");
    }
    group(terms)
}

/// Two equalities on one single-valued field match no record, so they are a
/// mistake rather than a query. `OR` is left alone: that is the shape the
/// message points at.
fn reject_repeated_field(terms: &[Condition]) -> Result<(), QueryError> {
    let mut seen: Vec<Field> = Vec::new();
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
