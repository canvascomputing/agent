//! Brave Search tool and response rendering for the research agents.

use agentwerk::tools::Tool;
use agentwerk::Event;
use serde_json::{json, Value};

const ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const MAX_RESULTS: u64 = 20;
const DESCRIPTION: &str = include_str!("prompts/web-search.tool.md");

pub(super) fn brave_key_from_env() -> Result<String, String> {
    match std::env::var("BRAVE_API_KEY") {
        Ok(key) if !key.trim().is_empty() => Ok(key.trim().to_string()),
        _ => Err("BRAVE_API_KEY is not set".to_string()),
    }
}

pub(super) fn brave_search_tool(api_key: String) -> Tool {
    Tool::new("brave_search")
        .description(DESCRIPTION)
        .schema(json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query." },
                "count": { "type": "integer", "description": "Results to return, from 1 to 20. Defaults to 5." }
            },
            "required": ["query"]
        }))
        .concurrent(true)
        .handler(move |input: Value| {
            let api_key = api_key.clone();
            async move { search(&api_key, &input).await }
        })
}

async fn search(api_key: &str, input: &Value) -> Event {
    let query = input["query"].as_str().unwrap_or_default().trim();
    if query.is_empty() {
        return Event::tool_call_failed("query must not be empty");
    }
    let count = input["count"]
        .as_u64()
        .unwrap_or(5)
        .clamp(1, MAX_RESULTS)
        .to_string();

    let response = match reqwest::Client::new()
        .get(ENDPOINT)
        .query(&[("q", query), ("count", &count)])
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return Event::tool_call_failed(format!("Brave search failed: {error}")),
    };
    if !response.status().is_success() {
        return Event::tool_call_failed(format!("Brave search returned {}", response.status()));
    }

    match response.json::<Value>().await {
        Ok(body) => Event::tool_call_finished(render_results(&body)),
        Err(error) => Event::tool_call_failed(format!("Brave returned invalid JSON: {error}")),
    }
}

fn render_results(body: &Value) -> String {
    let Some(results) = body["web"]["results"].as_array() else {
        return "No results found.".to_string();
    };
    if results.is_empty() {
        return "No results found.".to_string();
    }
    results
        .iter()
        .map(|result| {
            format!(
                "## {}\n{}\n{}",
                result["title"].as_str().unwrap_or_default(),
                result["url"].as_str().unwrap_or_default(),
                result["description"].as_str().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brave_results_include_titles_urls_and_descriptions() {
        let body = json!({ "web": { "results": [{
            "title": "Maintainable APIs",
            "url": "https://example.com/apis",
            "description": "A practical guide"
        }] } });

        assert_eq!(
            render_results(&body),
            "## Maintainable APIs\nhttps://example.com/apis\nA practical guide"
        );
    }

    #[test]
    fn missing_results_are_reported_plainly() {
        assert_eq!(render_results(&json!({})), "No results found.");
    }
}
