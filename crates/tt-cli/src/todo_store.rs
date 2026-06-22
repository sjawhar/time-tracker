use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tt_core::todos::{
    ParseDiagnostic, PriorityFile, StreamFile, TodoFile, TodoStore, parse_priorities,
    parse_streams, parse_todos,
};

use crate::Config;

const PRIORITIES_FILE: &str = "priorities.md";
const TODOS_FILE: &str = "todos.md";
const STREAMS_FILE: &str = "streams.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreDiagnostic {
    pub file: StoreFile,
    pub diagnostic: ParseDiagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreFile {
    Priorities,
    Todos,
    Streams,
}

impl StoreFile {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Priorities => PRIORITIES_FILE,
            Self::Todos => TODOS_FILE,
            Self::Streams => STREAMS_FILE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedTodoStore {
    pub store: TodoStore,
    pub diagnostics: Vec<StoreDiagnostic>,
}

pub fn store_dir(config: &Config) -> &Path {
    &config.todo_store_path
}

pub fn preflight_sync_conflicts(store_dir: &Path) -> Result<()> {
    if !store_dir.exists() {
        return Ok(());
    }

    let mut conflicts = Vec::new();
    collect_sync_conflicts(store_dir, &mut conflicts)?;
    conflicts.sort();

    if conflicts.is_empty() {
        return Ok(());
    }

    let conflict_list = conflicts
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    bail!("Syncthing conflict files found:\n{conflict_list}");
}

pub fn load_read_only(config: &Config) -> Result<LoadedTodoStore> {
    let dir = store_dir(config);
    preflight_sync_conflicts(dir)?;

    let priorities = read_optional_file(&dir.join(PRIORITIES_FILE))?;
    let todos = read_optional_file(&dir.join(TODOS_FILE))?;
    let streams = read_optional_file(&dir.join(STREAMS_FILE))?;

    Ok(parse_store_contents(&priorities, &todos, &streams))
}

pub fn load_mutating(config: &Config) -> Result<LoadedTodoStore> {
    let dir = store_dir(config);
    preflight_sync_conflicts(dir)?;
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create todo store directory {}", dir.display()))?;

    let priorities = read_optional_file(&dir.join(PRIORITIES_FILE))?;
    let todos = read_optional_file(&dir.join(TODOS_FILE))?;
    let streams = read_optional_file(&dir.join(STREAMS_FILE))?;

    Ok(parse_store_contents(&priorities, &todos, &streams))
}

pub fn write_priorities(config: &Config, priorities: &PriorityFile) -> Result<()> {
    write_store_file(
        &store_dir(config).join(PRIORITIES_FILE),
        &priorities.to_string(),
    )
}

pub fn write_todos(config: &Config, todos: &TodoFile) -> Result<()> {
    write_store_file(&store_dir(config).join(TODOS_FILE), &todos.to_string())
}

pub fn write_streams(config: &Config, streams: &StreamFile) -> Result<()> {
    write_store_file(&store_dir(config).join(STREAMS_FILE), &streams.to_string())
}

pub(crate) fn parse_store_contents(
    priorities: &str,
    todos: &str,
    streams: &str,
) -> LoadedTodoStore {
    let (priority_file, priority_diagnostics) = parse_priorities(priorities);
    let (todo_file, todo_diagnostics) = parse_todos(todos);
    let (stream_file, stream_diagnostics) = parse_streams(streams);

    LoadedTodoStore {
        store: TodoStore {
            priorities: priority_file,
            todos: todo_file,
            streams: stream_file,
        },
        diagnostics: collect_diagnostics(
            priority_diagnostics,
            todo_diagnostics,
            stream_diagnostics,
        ),
    }
}

fn collect_diagnostics(
    priority_diagnostics: Vec<ParseDiagnostic>,
    todo_diagnostics: Vec<ParseDiagnostic>,
    stream_diagnostics: Vec<ParseDiagnostic>,
) -> Vec<StoreDiagnostic> {
    let mut diagnostics = Vec::new();
    diagnostics.extend(tag_diagnostics(StoreFile::Priorities, priority_diagnostics));
    diagnostics.extend(tag_diagnostics(StoreFile::Todos, todo_diagnostics));
    diagnostics.extend(tag_diagnostics(StoreFile::Streams, stream_diagnostics));
    diagnostics
}

fn tag_diagnostics(
    file: StoreFile,
    diagnostics: Vec<ParseDiagnostic>,
) -> impl Iterator<Item = StoreDiagnostic> {
    diagnostics
        .into_iter()
        .map(move |diagnostic| StoreDiagnostic { file, diagnostic })
}

fn read_optional_file(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn write_store_file(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("todo store file path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create todo store directory {}", parent.display()))?;
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, contents).with_context(|| {
        format!(
            "failed to write temporary store file {}",
            temp_path.display()
        )
    })?;
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "failed to replace store file {} with {}",
            path.display(),
            temp_path.display()
        )
    })?;
    Ok(())
}

fn collect_sync_conflicts(dir: &Path, conflicts: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read todo store directory {}", dir.display()))?;

    for entry_result in entries {
        let entry = entry_result.with_context(|| {
            format!(
                "failed to read entry in todo store directory {}",
                dir.display()
            )
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect todo store path {}", path.display()))?;

        if file_type.is_dir() {
            collect_sync_conflicts(&path, conflicts)?;
        } else if is_sync_conflict_path(&path) {
            conflicts.push(path);
        }
    }

    Ok(())
}

fn is_sync_conflict_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(".sync-conflict-"))
}
