//! A ticket matcher that selects tickets by field values.

use super::ticket::{Status, Ticket};

/// A condition a ticket is tested against.
///
/// A label, [`Query`], and closures all implement this trait, so every method
/// that selects tickets accepts any of them. The blanket impl for
/// `Fn(&Ticket) -> bool` keeps closures working unchanged.
///
/// ```no_run
/// use agentwerk::{Query, TicketQueue};
///
/// let tickets = TicketQueue::new();
/// tickets.find_tickets("research");
/// tickets.find_tickets(Query::labeled("research").agent("research-1"));
/// tickets.find_tickets(|t: &agentwerk::Ticket| t.has_label("research"));
/// ```
pub trait TicketMatcher: Send + Sync {
    fn matches(&self, ticket: &Ticket) -> bool;
}

impl<F: Fn(&Ticket) -> bool + Send + Sync> TicketMatcher for F {
    fn matches(&self, ticket: &Ticket) -> bool {
        self(ticket)
    }
}

impl TicketMatcher for &str {
    fn matches(&self, ticket: &Ticket) -> bool {
        ticket.has_label(self)
    }
}

impl TicketMatcher for String {
    fn matches(&self, ticket: &Ticket) -> bool {
        ticket.has_label(self)
    }
}

/// Selects tickets by field values. Every set field must match (AND).
/// An empty query matches every ticket.
///
/// ```no_run
/// use agentwerk::Query;
///
/// let q = Query::labeled("scan").agent("scanner-1");
/// ```
#[derive(Debug, Clone, Default)]
pub struct Query {
    key: Option<String>,
    label: Option<String>,
    status: Option<Status>,
    agent: Option<String>,
    parent_key: Option<String>,
}

impl Query {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a query for one label, the field most queries select on.
    pub fn labeled(label: impl Into<String>) -> Self {
        Self::new().label(label)
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn status(mut self, status: Status) -> Self {
        self.status = Some(status);
        self
    }

    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    pub fn parent_key(mut self, parent_key: impl Into<String>) -> Self {
        self.parent_key = Some(parent_key.into());
        self
    }

    pub fn default_status(mut self, status: Status) -> Self {
        if self.status.is_none() {
            self.status = Some(status);
        }
        self
    }
}

impl From<&str> for Query {
    fn from(label: &str) -> Self {
        Query::labeled(label)
    }
}

impl From<String> for Query {
    fn from(label: String) -> Self {
        Query::labeled(label)
    }
}

impl TicketMatcher for Query {
    fn matches(&self, ticket: &Ticket) -> bool {
        self.key.as_ref().is_none_or(|k| ticket.key == *k)
            && self.label.as_ref().is_none_or(|l| ticket.has_label(l))
            && self.status.is_none_or(|s| ticket.status == s)
            && self
                .agent
                .as_ref()
                .is_none_or(|a| ticket.assignee.as_deref() == Some(a))
            && self
                .parent_key
                .as_ref()
                .is_none_or(|p| ticket.parent.as_deref() == Some(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(key: &str, label: Option<&str>, agent: Option<&str>, parent: Option<&str>) -> Ticket {
        let mut t = Ticket::new("task");
        t.key = key.to_string();
        t.label = label.map(str::to_string);
        t.assignee = agent.map(str::to_string);
        t.parent = parent.map(str::to_string);
        t
    }

    #[test]
    fn empty_query_matches_every_ticket() {
        let q = Query::new();
        assert!(q.matches(&ticket("TICKET-1", Some("scan"), Some("a-1"), None)));
        assert!(q.matches(&ticket("TICKET-2", None, None, None)));
    }

    #[test]
    fn key_filters_by_ticket_key() {
        let q = Query::new().key("TICKET-1");
        assert!(q.matches(&ticket("TICKET-1", None, None, None)));
        assert!(!q.matches(&ticket("TICKET-2", None, None, None)));
    }

    #[test]
    fn label_filters_by_ticket_label() {
        let q = Query::new().label("scan");
        assert!(q.matches(&ticket("TICKET-1", Some("scan"), None, None)));
        assert!(!q.matches(&ticket("TICKET-2", Some("report"), None, None)));
        assert!(!q.matches(&ticket("TICKET-3", None, None, None)));
    }

    #[test]
    fn status_filters_by_ticket_status() {
        let q = Query::new().status(Status::Finished);
        let mut t = ticket("TICKET-1", None, None, None);
        t.status = Status::Finished;
        assert!(q.matches(&t));
        t.status = Status::Todo;
        assert!(!q.matches(&t));
    }

    #[test]
    fn agent_filters_by_assignee() {
        let q = Query::new().agent("scanner-1");
        assert!(q.matches(&ticket("TICKET-1", None, Some("scanner-1"), None)));
        assert!(!q.matches(&ticket("TICKET-2", None, Some("scanner-2"), None)));
        assert!(!q.matches(&ticket("TICKET-3", None, None, None)));
    }

    #[test]
    fn parent_key_filters_by_parent() {
        let q = Query::new().parent_key("TICKET-1");
        assert!(q.matches(&ticket("TICKET-2", None, None, Some("TICKET-1"))));
        assert!(!q.matches(&ticket("TICKET-3", None, None, Some("TICKET-5"))));
        assert!(!q.matches(&ticket("TICKET-4", None, None, None)));
    }

    #[test]
    fn multiple_fields_and_together() {
        let q = Query::new().label("scan").agent("scanner-1");
        assert!(q.matches(&ticket("TICKET-1", Some("scan"), Some("scanner-1"), None)));
        assert!(!q.matches(&ticket("TICKET-2", Some("scan"), Some("scanner-2"), None)));
        assert!(!q.matches(&ticket("TICKET-3", Some("report"), Some("scanner-1"), None)));
    }

    #[test]
    fn default_status_sets_when_none() {
        let q = Query::new().default_status(Status::Finished);
        let mut t = ticket("TICKET-1", None, None, None);
        t.status = Status::Finished;
        assert!(q.matches(&t));
        t.status = Status::Todo;
        assert!(!q.matches(&t));
    }

    #[test]
    fn default_status_preserves_explicit_status() {
        let q = Query::new()
            .status(Status::Failed)
            .default_status(Status::Finished);
        let mut t = ticket("TICKET-1", None, None, None);
        t.status = Status::Failed;
        assert!(q.matches(&t));
        t.status = Status::Finished;
        assert!(!q.matches(&t));
    }

    #[test]
    fn closure_implements_ticket_matcher() {
        let matcher: &dyn TicketMatcher = &|t: &Ticket| t.has_label("scan");
        assert!(matcher.matches(&ticket("TICKET-1", Some("scan"), None, None)));
        assert!(!matcher.matches(&ticket("TICKET-2", Some("report"), None, None)));
    }

    #[test]
    fn labeled_filters_by_ticket_label() {
        let q = Query::labeled("scan");
        assert!(q.matches(&ticket("TICKET-1", Some("scan"), None, None)));
        assert!(!q.matches(&ticket("TICKET-2", Some("report"), None, None)));
    }

    #[test]
    fn str_implements_ticket_matcher_as_a_label() {
        let matcher: &dyn TicketMatcher = &"scan";
        assert!(matcher.matches(&ticket("TICKET-1", Some("scan"), None, None)));
        assert!(!matcher.matches(&ticket("TICKET-2", Some("report"), None, None)));
        assert!(!matcher.matches(&ticket("TICKET-3", None, None, None)));
    }

    #[test]
    fn string_implements_ticket_matcher_as_a_label() {
        let matcher: &dyn TicketMatcher = &"scan".to_string();
        assert!(matcher.matches(&ticket("TICKET-1", Some("scan"), None, None)));
        assert!(!matcher.matches(&ticket("TICKET-2", Some("report"), None, None)));
    }

    #[test]
    fn a_string_converts_into_a_label_query() {
        let q = Query::from("scan");
        assert!(q.matches(&ticket("TICKET-1", Some("scan"), None, None)));
        assert!(!q.matches(&ticket("TICKET-2", Some("report"), None, None)));
    }
}
