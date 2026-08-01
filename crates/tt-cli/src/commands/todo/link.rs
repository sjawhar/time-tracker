use anyhow::{Result, bail};
use tt_core::todos::TodoFileItem;

use super::mutate::unique_todo_line_index;
use super::stream_links::apply_todo_stream_links;
use crate::Config;
use crate::todo_store::{load_mutating, write_todos};

fn resolve_session_id(
    explicit: Option<String>,
    claude_env: Option<String>,
    opencode_env: Option<String>,
) -> Result<String> {
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    non_empty(explicit)
        .or_else(|| non_empty(claude_env))
        .or_else(|| non_empty(opencode_env))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no agent session detected (CLAUDE_CODE_SESSION_ID / OPENCODE_SESSION_ID unset); pass --session <id>"
            )
        })
}

fn session_from_env(explicit: Option<String>) -> Result<String> {
    resolve_session_id(
        explicit,
        std::env::var("CLAUDE_CODE_SESSION_ID").ok(),
        std::env::var("OPENCODE_SESSION_ID").ok(),
    )
}

/// Links an agent session to a todo, then applies the todo's stream to it.
///
/// The link is the mapping; applying it is what makes the mapping visible in the
/// timesheet. Doing both here is why no read command needs to write.
pub fn run_link(
    db: Option<&tt_db::Database>,
    config: &Config,
    id: &str,
    session: Option<String>,
) -> Result<()> {
    let session_id = session_from_env(session)?;
    let mut loaded = load_mutating(config)?;
    let index = unique_todo_line_index(&loaded, id)?;
    let TodoFileItem::Todo(todo) = &mut loaded.store.todos.items[index].item else {
        bail!("todo '{id}' not found");
    };
    if todo.sessions.iter().any(|linked| linked == &session_id) {
        println!("Already linked: {session_id} → {id} \"{}\"", todo.text);
        return Ok(());
    }
    todo.sessions.push(session_id.clone());
    let text = todo.text.clone();
    write_todos(config, &loaded.store.todos)?;
    println!("Linked {session_id} → {id} \"{text}\"");

    if let Some(db) = db {
        for note in apply_todo_stream_links(db, &loaded)? {
            eprintln!("{note}");
        }
    }
    Ok(())
}

pub fn run_unlink(config: &Config, id: &str, session: Option<String>) -> Result<()> {
    let session_id = session_from_env(session)?;
    let mut loaded = load_mutating(config)?;
    let index = unique_todo_line_index(&loaded, id)?;
    let TodoFileItem::Todo(todo) = &mut loaded.store.todos.items[index].item else {
        bail!("todo '{id}' not found");
    };
    let before = todo.sessions.len();
    todo.sessions.retain(|linked| linked != &session_id);
    if todo.sessions.len() == before {
        bail!("session {session_id} is not linked to todo '{id}'");
    }
    let text = todo.text.clone();
    write_todos(config, &loaded.store.todos)?;
    println!("Unlinked {session_id} from {id} \"{text}\"");
    Ok(())
}

#[cfg(test)]
mod tests {
    use tt_core::todos::TodoFileItem;

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

    fn sessions(config: &Config) -> Vec<String> {
        let loaded = load_read_only(config).unwrap();
        let TodoFileItem::Todo(todo) = &loaded.store.todos.items[0].item else {
            panic!("expected parsed todo");
        };
        todo.sessions.clone()
    }

    #[test]
    fn resolution_prefers_explicit_then_claude_then_opencode() {
        assert_eq!(
            resolve_session_id(Some("x".into()), Some("c".into()), Some("o".into())).unwrap(),
            "x"
        );
        assert_eq!(
            resolve_session_id(None, Some("c".into()), Some("o".into())).unwrap(),
            "c"
        );
        assert_eq!(
            resolve_session_id(None, None, Some("o".into())).unwrap(),
            "o"
        );
        let err = resolve_session_id(None, None, None).unwrap_err();
        assert!(err.to_string().contains("no agent session detected"));
    }

    #[test]
    fn resolution_ignores_empty_env_values() {
        assert_eq!(
            resolve_session_id(None, Some(String::new()), Some("o".into())).unwrap(),
            "o"
        );
    }

    #[test]
    fn link_appends_session_id_idempotently() {
        let (_temp, config) = fixture(
            "- [ ] Link task <!-- tt-todo:{\"id\":\"td_1\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
        );

        run_link(None, &config, "td_1", Some("ses_abc".into())).unwrap();
        run_link(None, &config, "td_1", Some("ses_abc".into())).unwrap();

        assert_eq!(sessions(&config), ["ses_abc"]);
    }

    #[test]
    fn unlink_removes_session_and_errors_when_absent() {
        let (_temp, config) = fixture(
            "- [ ] Unlink task <!-- tt-todo:{\"id\":\"td_1\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"ses_abc\"]} -->\n",
        );

        run_unlink(&config, "td_1", Some("ses_abc".into())).unwrap();
        assert!(sessions(&config).is_empty());

        let err = run_unlink(&config, "td_1", Some("ses_abc".into())).unwrap_err();
        assert!(err.to_string().contains("not linked"));
    }
}
