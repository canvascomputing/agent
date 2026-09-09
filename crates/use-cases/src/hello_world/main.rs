//! The smallest agentwerk program: one agent, one task, one answer.
//!
//! Builds an agent from the environment, submits a single task, waits
//! until no task remains, and prints the result. No tools, no labels, no
//! schema.
//!
//! Usage: hello-world [TASK]
//!
//! Example:
//!   hello-world
//!   hello-world "Greet the world in Japanese."

use agentwerk::Agent;

const DEFAULT_TASK: &str = "Say hello to the world in one short sentence.";

#[tokio::main]
async fn main() {
    let task = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_TASK.into());

    let agent = Agent::from_env().role(
        "You are a friendly greeter. Finish with an `answer` field containing one short sentence.",
    );

    agent.add_task(task);

    let mut results = agent.finish().await;

    match results.pop() {
        Some(result) => println!("{}", result["answer"].as_str().unwrap_or_default()),
        None => {
            eprintln!("the agent finished no task");
            std::process::exit(1);
        }
    }
}
