//! Route a draft through an editor with a result hook, then select it with AQL.
//!
//! Usage: editorial-review [TEXT]

use agentwerk::{Agent, Task, Werk};

const DRAFT: &str = "draft";
const EDIT: &str = "edit";
const FINAL_EDIT: &str = "task.label = edit AND task.status = finished";
const DEFAULT_TEXT: &str = "Announce a new software release in two sentences.";

#[tokio::main]
async fn main() {
    let text = text_from_args();
    let werk = Werk::new();

    werk.add_agent(
        Agent::from_env()
            .label(DRAFT)
            .role("Write the requested draft. Return only the drafted text."),
    );
    werk.add_agent(
        Agent::from_env()
            .label(EDIT)
            .role("Edit the draft for clarity and brevity. Return only the final text."),
    );
    werk.on_result(|werk, task, result| {
        if task.get_label() == Some(DRAFT) {
            werk.add_task(Task::labeled(EDIT, result.clone()));
        }
    });

    werk.add_task(Task::labeled(DRAFT, text));
    werk.finish().await;

    match werk.find_result(FINAL_EDIT) {
        Some(result) => println!("{}", result.as_str().unwrap_or_default()),
        None => {
            eprintln!("the editor produced no result");
            std::process::exit(1);
        }
    }
}

fn text_from_args() -> String {
    let text = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        DEFAULT_TEXT.to_string()
    } else {
        text
    }
}
