//! The [`Trajectory`] value type: a task's replies captured as a
//! single training example, plus its disk persistence.

use std::io;
use std::path::{Path, PathBuf};

use super::{Author, Reply, ReplyContent, Task};

/// A finished agent run reduced to the one thing a training example needs:
/// the messages, in agentwerk's own [`Reply`] shape. Build one with
/// [`Trajectory::from_task`] from an [`on_task`] handler and [`save`] it
/// where the dataset belongs, leaving any ShareGPT / chat_template conversion
/// to a downstream step.
///
/// [`on_task`]: super::Queue::on_task
/// [`save`]: Trajectory::save
///
/// Each save also writes an `.html` sibling rendering the messages for
/// debugging.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Trajectory {
    /// Example id `<agent id>-<task>`; also the on-disk filename.
    pub(crate) key: String,
    /// Name of the model that produced the replies. `None` when the
    /// producing agent could not be resolved at capture time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    /// Every reply exchanged with the model.
    pub(crate) replies: Vec<Reply>,
}

impl Trajectory {
    /// The example identifier, also used as its filename.
    pub fn get_key(&self) -> &str {
        &self.key
    }

    /// The model that produced the replies, if known.
    pub fn get_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Every reply exchanged with the model.
    pub fn get_replies(&self) -> &[Reply] {
        &self.replies
    }

    /// Capture `task`'s replies as an example produced by `agent_id`
    /// using `model`. Keeps every reply, including the system prompt: a
    /// trainer wants it, where `Task::to_messages` would drop it.
    pub fn from_task(agent_id: &str, model: Option<&str>, task: &Task) -> Self {
        Self {
            key: format!("{agent_id}-{}", task.key),
            model: model.map(str::to_string),
            replies: task.replies.clone(),
        }
    }

    /// Write the example under `dir` as `trajectories/<key>.json`, plus the
    /// `.html` sibling.
    pub fn save(&self, dir: impl AsRef<Path>) -> io::Result<()> {
        <Self as crate::persistence::Persist>::save(self, dir.as_ref())
    }

    /// Render the messages as a self-contained, readable HTML page:
    /// one role-colored card per turn, tool input and output folded into
    /// `<details>`. Message text is escaped, not parsed as markdown, so the
    /// page needs nothing else and shows every message as it was.
    fn to_html(&self) -> String {
        // Entity-escape so a payload containing `<script>` renders as text.
        fn escape(text: &str) -> String {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        }

        let mut out = format!("{HTML_HEAD}<h1>{}</h1>\n", escape(&self.key));
        if let Some(model) = &self.model {
            out.push_str(&format!(
                "<p class=\"model\">model: {}</p>\n",
                escape(model)
            ));
        }
        for reply in &self.replies {
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
                    ReplyContent::Text { text } => {
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
fn trajectory_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("trajectories").join(format!("{key}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::Persist;
    use crate::test_util::TempDir;

    fn task_with_reply() -> Task {
        let mut task = Task::new("scan the file");
        task.key = "t-1".into();
        task.replies.push(Reply::user_text("hello"));
        task
    }

    #[test]
    fn from_task_carries_replies() {
        let trajectory = Trajectory::from_task("analyst", Some("gpt-4"), &task_with_reply());
        assert_eq!(trajectory.get_key(), "analyst-t-1");
        assert_eq!(trajectory.get_model(), Some("gpt-4"));
        assert_eq!(trajectory.get_replies().len(), 1);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let trajectory = Trajectory::from_task("analyst", Some("gpt-4"), &task_with_reply());
        trajectory.save(dir.path()).unwrap();

        let path = dir.path().join("trajectories").join("analyst-t-1.json");
        assert!(path.exists());
        let loaded = Trajectory::load(dir.path(), &trajectory.key).unwrap();
        assert_eq!(loaded.key, trajectory.key);
        assert_eq!(loaded.model.as_deref(), Some("gpt-4"));
        assert_eq!(loaded.replies.len(), 1);
    }

    #[test]
    fn save_writes_html_sibling() {
        let dir = TempDir::new().unwrap();
        let mut trajectory = Trajectory::from_task("analyst", Some("gpt-4"), &task_with_reply());
        trajectory.replies.push(Reply {
            author: Author::Assistant,
            content: vec![ReplyContent::ToolUse {
                id: "t1".into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": "a.txt" }),
            }],
            created_at: 0,
        });
        trajectory.save(dir.path()).unwrap();

        let path = dir.path().join("trajectories").join("analyst-t-1.html");
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("<div class=\"turn user\">"));
        assert!(html.contains("hello"));
        assert!(html.contains("<summary>&rarr; read_file</summary>"));
        assert!(html.contains("<p class=\"model\">model: gpt-4</p>"));
    }

    #[test]
    fn save_writes_html_without_model_line_when_model_is_none() {
        let dir = TempDir::new().unwrap();
        let trajectory = Trajectory::from_task("analyst", None, &task_with_reply());
        trajectory.save(dir.path()).unwrap();

        let path = dir.path().join("trajectories").join("analyst-t-1.html");
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(!html.contains("class=\"model\""));
    }

    #[test]
    fn html_escapes_message_text() {
        let dir = TempDir::new().unwrap();
        let mut trajectory = Trajectory::from_task("analyst", Some("gpt-4"), &task_with_reply());
        trajectory
            .replies
            .push(Reply::user_text("<script>alert(1)</script>"));
        trajectory.save(dir.path()).unwrap();

        let html =
            std::fs::read_to_string(dir.path().join("trajectories").join("analyst-t-1.html"))
                .unwrap();
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)"));
    }
}
