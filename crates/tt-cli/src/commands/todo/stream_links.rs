//! Applying a todo's stream to the agent sessions that todo links.
//!
//! A todo naming both a stream slug and a session id is a mapping the human wrote by
//! hand, so propagating it to that session's events infers nothing — it just moves an
//! answer the human already gave onto the rows that need it. The events are stamped
//! `todo_link` so they stay distinguishable from both machine inference and a direct
//! `tt streams assign`.
//!
//! The sweep runs whenever a link is created, because that is the moment the mapping
//! changes. It is idempotent, so running it again costs nothing.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::todo_store::LoadedTodoStore;
use tt_core::todos::TodoFileItem;

/// A todo that names at least one agent session.
#[derive(Clone, Debug)]
struct LinkedTodo {
    id: String,
    stream_slug: Option<String>,
}

/// Maps each linked session id to the todo that claims it.
///
/// A session linked by two todos is ambiguous; the first wins and the duplicate is
/// reported rather than silently picked.
fn session_todo_index(loaded: &LoadedTodoStore) -> HashMap<String, LinkedTodo> {
    let mut index: HashMap<String, LinkedTodo> = HashMap::new();

    for line in &loaded.store.todos.items {
        let TodoFileItem::Todo(todo) = &line.item else {
            continue;
        };
        if todo.done {
            continue;
        }

        let linked_todo = LinkedTodo {
            id: todo.id.clone(),
            stream_slug: todo.stream.clone(),
        };
        for session_id in &todo.sessions {
            if let Some(first_todo) = index.get(session_id) {
                if first_todo.id != todo.id {
                    eprintln!(
                        "todo {} duplicates session {session_id} linked by todo {}; keeping first link",
                        todo.id, first_todo.id
                    );
                }
                continue;
            }
            index.insert(session_id.clone(), linked_todo.clone());
        }
    }

    index
}

/// Assigns each linked session's events to the stream its todo names.
///
/// Returns notes about links that could not be applied. A todo naming a slug no stream
/// carries is reported rather than creating one — minting a container from a typo is
/// exactly the failure the classifier's name guard exists to prevent.
fn apply_index(db: &tt_db::Database, index: &HashMap<String, LinkedTodo>) -> Result<Vec<String>> {
    let mut session_ids: Vec<_> = index.keys().collect();
    session_ids.sort_unstable();

    let mut notes = Vec::new();
    for session_id in session_ids {
        let linked_todo = &index[session_id];
        let Some(slug) = &linked_todo.stream_slug else {
            continue;
        };

        match db
            .get_stream_by_slug(slug)
            .with_context(|| format!("failed to query stream slug '{slug}'"))?
        {
            Some(stream) => {
                db.assign_events_by_session_id(session_id, &stream.id, "todo_link")
                    .with_context(|| {
                        format!(
                            "failed to assign events for linked session {session_id} to stream '{slug}'"
                        )
                    })?;
            }
            None => notes.push(format!(
                "todo {} references slug '{slug}' with no matching stream; session {session_id} left unclassified",
                linked_todo.id
            )),
        }
    }

    Ok(notes)
}

/// Applies every todo→stream link in the store to its sessions' events.
pub(super) fn apply_todo_stream_links(
    db: &tt_db::Database,
    loaded: &LoadedTodoStore,
) -> Result<Vec<String>> {
    apply_index(db, &session_todo_index(loaded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::todo_store::load_read_only;

    fn fixture(todos: &str) -> (tempfile::TempDir, Config) {
        let temp = tempfile::TempDir::new().unwrap();
        let store = temp.path().join("todo-store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("todos.md"), todos).unwrap();
        let config = Config {
            database_path: temp.path().join("tt.db"),
            todo_store_path: store,
            ..Config::default()
        };
        (temp, config)
    }

    fn todo_line(id: &str, stream: &str, sessions: &str) -> String {
        format!(
            "- [ ] {id} <!-- tt-todo:{{\"id\":\"{id}\",\"priority\":[],\"stream\":{stream},\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":{sessions}}} -->\n"
        )
    }

    fn stream(id: &str, slug: &str) -> tt_db::Stream {
        let now = chrono::Utc::now();
        tt_db::Stream {
            id: id.to_string(),
            created_at: now,
            updated_at: now,
            name: Some(slug.to_string()),
            slug: Some(slug.to_string()),
            description: None,
            color: None,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        }
    }

    fn event(id: &str, session_id: &str) -> tt_db::StoredEvent {
        tt_db::StoredEvent {
            id: id.to_string(),
            timestamp: chrono::Utc::now(),
            event_type: tt_core::EventType::AgentToolUse,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            cwd: Some("/project".to_string()),
            git_project: None,
            git_workspace: None,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            status: None,
            idle_duration_ms: None,
            action: None,
            session_id: Some(session_id.to_string()),
            stream_id: None,
            assignment_source: None,
            data: serde_json::Value::Null,
            window_app_id: None,
            window_title: None,
        }
    }

    #[test]
    fn applies_a_matching_slug_and_reports_one_that_matches_nothing() {
        // Given: two linked todos — one naming a real stream, one naming a typo.
        let (_temp, config) = fixture(&format!(
            "{}{}",
            todo_line("td_1", "\"real\"", "[\"ses-a\"]"),
            todo_line("td_2", "\"typo\"", "[\"ses-b\"]"),
        ));
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_stream(&stream("s1", "real")).unwrap();
        db.insert_event(&event("e1", "ses-a")).unwrap();
        db.insert_event(&event("e2", "ses-b")).unwrap();

        // When: the links are applied.
        let notes = apply_todo_stream_links(&db, &load_read_only(&config).unwrap()).unwrap();

        // Then: the real one lands as `todo_link`, and the typo is reported, not minted.
        let assigned = db.get_events_by_stream("s1").unwrap();
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].assignment_source.as_deref(), Some("todo_link"));
        assert_eq!(db.get_streams().unwrap().len(), 1);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("slug 'typo' with no matching stream"));
    }

    #[test]
    fn never_overwrites_a_human_assignment() {
        // Given: a session a human already filed, and a todo pointing elsewhere.
        let (_temp, config) = fixture(&todo_line("td_1", "\"other\"", "[\"ses-a\"]"));
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_stream(&stream("s1", "other")).unwrap();
        db.insert_stream(&stream("s2", "human")).unwrap();
        db.insert_event(&event("e1", "ses-a")).unwrap();
        db.reassign_session_as_user("ses-a", "s2").unwrap();

        // When/Then: the sweep leaves the human's verdict alone.
        apply_todo_stream_links(&db, &load_read_only(&config).unwrap()).unwrap();
        assert!(db.get_events_by_stream("s1").unwrap().is_empty());
        assert_eq!(db.get_events_by_stream("s2").unwrap().len(), 1);
    }

    #[test]
    fn skips_done_todos_and_todos_with_no_stream() {
        // Given: a streamless todo and a done one.
        let (_temp, config) = fixture(&format!(
            "{}{}",
            todo_line("td_1", "null", "[\"ses-a\"]"),
            "- [x] done <!-- tt-todo:{\"id\":\"td_2\",\"priority\":[],\"stream\":\"real\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"ses-b\"]} -->\n",
        ));
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_stream(&stream("s1", "real")).unwrap();
        db.insert_event(&event("e1", "ses-a")).unwrap();
        db.insert_event(&event("e2", "ses-b")).unwrap();

        // When/Then: neither is applied.
        let notes = apply_todo_stream_links(&db, &load_read_only(&config).unwrap()).unwrap();
        assert!(notes.is_empty());
        assert!(db.get_events_by_stream("s1").unwrap().is_empty());
    }
}
