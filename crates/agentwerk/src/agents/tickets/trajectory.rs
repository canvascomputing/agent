//! The [`Trajectory`] value type: a ticket's transcript captured as a
//! single training example, plus its disk persistence.

use std::io;
use std::path::{Path, PathBuf};

use super::{Reply, Ticket};

/// A finished agent run reduced to the one thing a training example needs:
/// the message transcript, in agentwerk's own [`Reply`] shape. Written
/// selectively by [`TicketSystem::save_trajectory_on_event`], leaving any
/// ShareGPT / chat_template conversion to a downstream step.
///
/// [`TicketSystem::save_trajectory_on_event`]: super::TicketSystem::save_trajectory_on_event
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Trajectory {
    /// Example id `<agent>-<ticket>`; also the on-disk filename.
    pub key: String,
    /// The transcript exchanged with the model.
    pub messages: Vec<Reply>,
}

impl Trajectory {
    /// Capture `ticket`'s transcript as an example produced by `agent`.
    /// Keeps every reply, including the system prompt: a trainer wants it,
    /// where `Ticket::to_messages` would drop it.
    pub(crate) fn from_ticket(agent: &str, ticket: &Ticket) -> Self {
        Self {
            key: format!("{agent}-{}", ticket.key),
            messages: ticket.replies.clone(),
        }
    }
}

impl crate::persistence::Persist for Trajectory {
    type Key = String;

    fn save(&self, dir: &Path) -> io::Result<()> {
        let path = trajectory_path(dir, &self.key);
        let body = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        crate::persistence::write_atomic(&path, &body)
    }

    fn load(dir: &Path, key: &Self::Key) -> io::Result<Self> {
        let bytes = std::fs::read(trajectory_path(dir, key))?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}

/// Path of the trajectory file for `key`: `trajectories/<key>.json`.
pub(super) fn trajectory_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("trajectories").join(format!("{key}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::Persist;
    use crate::test_util::TempDir;

    fn ticket_with_reply() -> Ticket {
        let mut ticket = Ticket::new("scan the file");
        ticket.key = "TICKET-1".into();
        ticket.replies.push(Reply::user_text("hello"));
        ticket
    }

    #[test]
    fn from_ticket_carries_replies() {
        let trajectory = Trajectory::from_ticket("analyst", &ticket_with_reply());
        assert_eq!(trajectory.key, "analyst-TICKET-1");
        assert_eq!(trajectory.messages.len(), 1);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let trajectory = Trajectory::from_ticket("analyst", &ticket_with_reply());
        trajectory.save(dir.path()).unwrap();

        let path = dir
            .path()
            .join("trajectories")
            .join("analyst-TICKET-1.json");
        assert!(path.exists());
        let loaded = Trajectory::load(dir.path(), &trajectory.key).unwrap();
        assert_eq!(loaded.key, trajectory.key);
        assert_eq!(loaded.messages.len(), 1);
    }
}
