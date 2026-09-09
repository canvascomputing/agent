//! Minimal streaming chat with an interactive agent.
//!
//! `/new` closes the current conversation. `/quit` exits.

use std::io::{self, Write};

use agentwerk::{Agent, Event, Werk};

#[tokio::main]
async fn main() {
    let agent = Agent::from_env()
        .interactive()
        .role("Answer clearly and concisely.");
    let werk = agent.start();
    werk.on_event(|_, event| stream_text(event));

    println!("agentwerk chat: /new starts over, /quit exits");
    let mut chat_id = None;

    loop {
        let Some(input) = read_line() else {
            close_chat(&werk, &mut chat_id);
            break;
        };
        match input.as_str() {
            "" => continue,
            "/new" => {
                close_chat(&werk, &mut chat_id);
                println!("new conversation");
                continue;
            }
            "/quit" => {
                close_chat(&werk, &mut chat_id);
                break;
            }
            _ => {}
        }

        print!("agent> ");
        let _ = io::stdout().flush();
        let id = match active_chat(&werk, chat_id.as_deref()) {
            Some(id) => {
                werk.add_reply(id, input);
                id.to_string()
            }
            None => agent.add_task(input),
        };
        chat_id = Some(id.clone());
        werk.finish_task(id).await;
        println!();
    }
}

fn read_line() -> Option<String> {
    print!("you> ");
    io::stdout().flush().ok()?;

    let mut input = String::new();
    (io::stdin().read_line(&mut input).ok()? > 0).then(|| input.trim().to_string())
}

fn stream_text(event: &Event) {
    if event.get_name() == Event::TEXT_CHUNK_RECEIVED {
        print!(
            "{}",
            event.get_data()["content"].as_str().unwrap_or_default()
        );
        let _ = io::stdout().flush();
    }
}

fn active_chat<'a>(werk: &Werk, id: Option<&'a str>) -> Option<&'a str> {
    id.filter(|id| werk.get_task(id).is_some_and(|task| task.is_in_progress()))
}

fn close_chat(werk: &Werk, id: &mut Option<String>) {
    if let Some(id) = id.take() {
        if werk.get_task(&id).is_some_and(|task| task.is_pending()) {
            let _ = werk.set_task_finished(&id, "closed");
        }
    }
}
