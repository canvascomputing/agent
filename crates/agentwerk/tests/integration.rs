//! Integration tests that hit a live LLM provider.
//! Run with provider env vars set (e.g. `ANTHROPIC_API_KEY` + `MODEL`).

#[path = "integration/common.rs"]
mod common;

#[path = "../src/test_util.rs"]
mod test_util;

#[path = "integration/command_usage.rs"]
mod command_usage;

#[path = "integration/grep_finds_by_shape.rs"]
mod grep_finds_by_shape;

#[path = "integration/seeker_finds_planted.rs"]
mod seeker_finds_planted;

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

#[path = "integration/tasks_all_actions.rs"]
mod tasks_all_actions;

#[path = "integration/thinking_capture.rs"]
mod thinking_capture;
