use std::fmt::Write;

use anyhow::{Context, Result};
use serde::Serialize;
use tt_core::todos::{AlignmentFinding, find_alignment};

use crate::commands::todo::view::{priority_items, stream_links, todo_items};
use crate::todo_store::LoadedTodoStore;

#[derive(Debug, Serialize)]
struct JsonCheck {
    findings: Vec<JsonFinding>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum JsonFinding {
    Pinned {
        index: usize,
        todo_id: String,
        rank: Option<i32>,
    },
    Misordered {
        index: usize,
        todo_id: String,
        rank: Option<i32>,
        previous_rank: Option<i32>,
    },
    OrphanedLinks {
        index: usize,
        todo_id: String,
    },
    DuplicateStreamLink {
        stream: String,
        priorities: Vec<String>,
    },
}

pub fn render_check(loaded: &LoadedTodoStore, json: bool) -> Result<String> {
    let priorities = priority_items(loaded);
    let stream_links = stream_links(loaded);
    let todos = todo_items(loaded);
    let findings = find_alignment(&todos, &priorities, &stream_links);
    if json {
        render_json(&findings)
    } else {
        render_human(&findings)
    }
}

fn render_json(findings: &[AlignmentFinding]) -> Result<String> {
    let check = JsonCheck {
        findings: findings.iter().map(json_finding).collect(),
    };
    let mut output =
        serde_json::to_string_pretty(&check).context("failed to serialize check JSON")?;
    output.push('\n');
    Ok(output)
}

fn json_finding(finding: &AlignmentFinding) -> JsonFinding {
    match finding {
        AlignmentFinding::Pinned {
            index,
            todo_id,
            rank,
        } => JsonFinding::Pinned {
            index: *index,
            todo_id: todo_id.clone(),
            rank: *rank,
        },
        AlignmentFinding::Misordered {
            index,
            todo_id,
            rank,
            previous_rank,
        } => JsonFinding::Misordered {
            index: *index,
            todo_id: todo_id.clone(),
            rank: *rank,
            previous_rank: *previous_rank,
        },
        AlignmentFinding::OrphanedLinks { index, todo_id } => JsonFinding::OrphanedLinks {
            index: *index,
            todo_id: todo_id.clone(),
        },
        AlignmentFinding::DuplicateStreamLink { stream, priorities } => {
            JsonFinding::DuplicateStreamLink {
                stream: stream.clone(),
                priorities: priorities.clone(),
            }
        }
    }
}

fn render_human(findings: &[AlignmentFinding]) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "TODO CHECK").context("failed to format check header")?;
    if findings.is_empty() {
        writeln!(output, "No alignment findings.").context("failed to format empty check")?;
        return Ok(output);
    }
    for finding in findings {
        match finding {
            AlignmentFinding::Pinned {
                index,
                todo_id,
                rank,
            } => writeln!(
                output,
                "- pinned: {todo_id} at line {} rank {}",
                index + 1,
                rank_label(*rank)
            )
            .context("failed to format pinned finding")?,
            AlignmentFinding::Misordered {
                index,
                todo_id,
                rank,
                previous_rank,
            } => writeln!(
                output,
                "- misordered: {todo_id} at line {} rank {} follows {}",
                index + 1,
                rank_label(*rank),
                rank_label(*previous_rank)
            )
            .context("failed to format misordered finding")?,
            AlignmentFinding::OrphanedLinks { index, todo_id } => writeln!(
                output,
                "- orphaned: {todo_id} at line {} links only inactive or missing priorities",
                index + 1
            )
            .context("failed to format orphaned finding")?,
            AlignmentFinding::DuplicateStreamLink { stream, priorities } => writeln!(
                output,
                "- duplicate stream link: {stream} is linked to priorities {}",
                priorities.join(", ")
            )
            .context("failed to format duplicate stream link finding")?,
        }
    }
    Ok(output)
}

fn rank_label(rank: Option<i32>) -> String {
    rank.map_or_else(|| "-".to_string(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo_store::parse_store_contents;

    const PRIORITIES: &str = "- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n- [ ] Admin <!-- tt-priority:{\"slug\":\"admin\",\"value\":2,\"status\":\"active\"} -->\n";
    const TODOS: &str = "- [ ] Shared stream work <!-- tt-todo:{\"id\":\"td_shared001\",\"priority\":[],\"stream\":\"Shared stream\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    const DUPLICATE_STREAMS: &str = "- Shared stream <!-- tt-stream:{\"priority\":\"ipi\"} -->\n- Shared stream <!-- tt-stream:{\"priority\":\"admin\"} -->\n";

    #[test]
    fn check_human_reports_duplicate_stream_links() {
        // Given: streams.md links one stream name to two priorities.
        let loaded = parse_store_contents(PRIORITIES, TODOS, DUPLICATE_STREAMS);

        // When: todo check is rendered for humans.
        let output = render_check(&loaded, false).unwrap();

        // Then: the duplicate link is surfaced with both priority slugs.
        assert!(
            output.contains(
                "- duplicate stream link: Shared stream is linked to priorities ipi, admin"
            ),
            "duplicate stream link finding missing:\n{output}"
        );
    }

    #[test]
    fn check_json_reports_duplicate_stream_links() {
        // Given: streams.md links one stream name to two priorities.
        let loaded = parse_store_contents(PRIORITIES, TODOS, DUPLICATE_STREAMS);

        // When: todo check is rendered as JSON.
        let output = render_check(&loaded, true).unwrap();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();

        // Then: JSON includes a structured duplicate_stream_link finding.
        assert_eq!(json["findings"][0]["type"], "duplicate_stream_link");
        assert_eq!(json["findings"][0]["stream"], "Shared stream");
        assert_eq!(json["findings"][0]["priorities"][0], "ipi");
        assert_eq!(json["findings"][0]["priorities"][1], "admin");
    }
}
