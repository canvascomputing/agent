//! The [`Trajectory`] value type: a ticket's transcript captured as a
//! single training example, plus its disk persistence.

use std::io;
use std::path::{Path, PathBuf};

use super::{Author, Reply, ReplyContent, Ticket};

/// A finished agent run reduced to the one thing a training example needs:
/// the message transcript, in agentwerk's own [`Reply`] shape. Written
/// selectively by [`TicketSystem::save_trajectory_on_event`], leaving any
/// ShareGPT / chat_template conversion to a downstream step.
///
/// [`TicketSystem::save_trajectory_on_event`]: super::TicketSystem::save_trajectory_on_event
///
/// Each save also writes an `.html` sibling rendering the transcript for
/// debugging.
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

    /// Render the transcript as a self-contained, debug-readable HTML page:
    /// one role-colored card per turn, tool input and output folded into
    /// `<details>`. Message text is escaped, not parsed as markdown, so the
    /// page stays dependency-free and shows the transcript verbatim.
    fn to_html(&self) -> String {
        // Entity-escape so a payload containing `<script>` renders as text.
        fn escape(text: &str) -> String {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        }

        let mut out = format!("{HTML_HEAD}<h1>{}</h1>\n", escape(&self.key));
        for reply in &self.messages {
            let role = match reply.author {
                Author::System => "system",
                Author::User => "user",
                Author::Assistant => "assistant",
            };
            out.push_str(&format!(
                "<div class=\"turn {role}\"><div class=\"role\">{role}</div>\n"
            ));
            for block in &reply.content {
                match block {
                    ReplyContent::Text(text) => {
                        out.push_str(&format!("<pre>{}</pre>\n", escape(text)))
                    }
                    ReplyContent::Thinking { thinking, .. } => out.push_str(&format!(
                        "<pre class=\"think\">{}</pre>\n",
                        escape(thinking)
                    )),
                    ReplyContent::ToolUse { name, input, .. } => {
                        let json = serde_json::to_string_pretty(input)
                            .unwrap_or_else(|_| input.to_string());
                        out.push_str(&format!(
                            "<details><summary>&rarr; {}</summary><pre>{}</pre></details>\n",
                            escape(name),
                            escape(&json)
                        ));
                    }
                    ReplyContent::ToolResult {
                        content,
                        succeeded,
                        path,
                        ..
                    } => {
                        let marker = if *succeeded { "ok" } else { "err" };
                        let mut body = escape(content);
                        if let Some(path) = path {
                            body.push_str(&format!(
                                "\n\nFull output: {}",
                                escape(&path.display().to_string())
                            ));
                        }
                        out.push_str(&format!(
                            "<details><summary>&larr; {marker}</summary><pre>{body}</pre></details>\n"
                        ));
                    }
                    ReplyContent::RedactedThinking { .. } => {
                        out.push_str("<pre class=\"think\">redacted thinking</pre>\n")
                    }
                }
            }
            out.push_str("</div>\n");
        }
        out.push_str("</body></html>\n");
        out
    }
}

/// Opening tags and inline stylesheet for the trajectory HTML page. Inlined
/// so the file opens in a browser with no external assets.
const HTML_HEAD: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<style>
  body{font:14px/1.5 ui-monospace,monospace;max-width:60rem;margin:2rem auto;background:#0d1117;color:#c9d1d9}
  .turn{border-left:4px solid;padding:.5rem 1rem;margin:1rem 0;border-radius:6px;background:#161b22}
  .user{border-color:#3fb950}.assistant{border-color:#58a6ff}.system{border-color:#8b949e}
  .role{display:inline-block;font-weight:700;text-transform:uppercase;font-size:.72rem;letter-spacing:.06em;padding:.15rem .55rem;border-radius:999px;margin-bottom:.5rem;color:#0d1117}
  .user .role{background:#3fb950}.assistant .role{background:#58a6ff}.system .role{background:#8b949e}
  .think{color:#8b949e;font-style:italic}
  details{margin:.5rem 0}summary{cursor:pointer;color:#d29922}
  pre{white-space:pre-wrap;overflow-x:auto;margin:.5rem 0}
</style></head><body>
"#;

impl crate::persistence::Persist for Trajectory {
    type Key = String;

    fn save(&self, dir: &Path) -> io::Result<()> {
        let path = trajectory_path(dir, &self.key);
        let body = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        crate::persistence::write_atomic(&path, &body)?;
        crate::persistence::write_atomic(&path.with_extension("html"), self.to_html().as_bytes())
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

    #[test]
    fn save_writes_html_sibling() {
        let dir = TempDir::new().unwrap();
        let mut trajectory = Trajectory::from_ticket("analyst", &ticket_with_reply());
        trajectory.messages.push(Reply {
            author: Author::Assistant,
            content: vec![ReplyContent::ToolUse {
                id: "t1".into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": "a.txt" }),
            }],
            created_at: 0,
        });
        trajectory.save(dir.path()).unwrap();

        let path = dir
            .path()
            .join("trajectories")
            .join("analyst-TICKET-1.html");
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("<div class=\"turn user\">"));
        assert!(html.contains("hello"));
        assert!(html.contains("<summary>&rarr; read_file</summary>"));
    }

    #[test]
    fn html_escapes_message_text() {
        let dir = TempDir::new().unwrap();
        let mut trajectory = Trajectory::from_ticket("analyst", &ticket_with_reply());
        trajectory
            .messages
            .push(Reply::user_text("<script>alert(1)</script>"));
        trajectory.save(dir.path()).unwrap();

        let html = std::fs::read_to_string(
            dir.path()
                .join("trajectories")
                .join("analyst-TICKET-1.html"),
        )
        .unwrap();
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)"));
    }
}
