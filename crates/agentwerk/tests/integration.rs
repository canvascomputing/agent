//! Integration tests that hit a live LLM provider.
//! Run with provider env vars set (e.g. `ANTHROPIC_API_KEY` + `MODEL`).

#[path = "integration/common.rs"]
mod common;

#[path = "../src/test_util.rs"]
mod test_util;

#[path = "integration/bash_usage.rs"]
mod bash_usage;

#[path = "integration/codegrep_finds_by_shape.rs"]
mod codegrep_finds_by_shape;

#[path = "integration/threat_researcher_finds_planted.rs"]
mod threat_researcher_finds_planted;

#[path = "integration/compaction.rs"]
mod compaction;

#[path = "integration/edit_file_replaces_content.rs"]
mod edit_file_replaces_content;

#[path = "integration/file_exploration.rs"]
mod file_exploration;

#[path = "integration/glob_finds_nested_files.rs"]
mod glob_finds_nested_files;

#[path = "integration/grep_content_output.rs"]
mod grep_content_output;

#[path = "integration/grep_finds_code_pattern.rs"]
mod grep_finds_code_pattern;

#[path = "integration/list_directory_enumerates_entries.rs"]
mod list_directory_enumerates_entries;

#[path = "integration/traces_call_path_across_files.rs"]
mod traces_call_path_across_files;

#[path = "integration/write_file_creates_file.rs"]
mod write_file_creates_file;

#[path = "integration/find_tools_discovers_deferred.rs"]
mod find_tools_discovers_deferred;

#[path = "integration/manage_tickets_creates_ticket.rs"]
mod manage_tickets_creates_ticket;

#[path = "integration/read_tickets_finds_seeded.rs"]
mod read_tickets_finds_seeded;
