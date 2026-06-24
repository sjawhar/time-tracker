use chrono::NaiveDate;
use tt_core::todos::{
    Priority, PriorityFileItem, PriorityStatus, StreamFileItem, StreamPriorityLink, Todo,
    TodoFileItem, classify_next_sections, parse_todo_lenient, priority_rank,
};

use crate::commands::todo::NextOptions;
use crate::todo_store::LoadedTodoStore;

#[derive(Debug)]
pub struct TodoView<'a> {
    pub loaded: &'a LoadedTodoStore,
    pub today: NaiveDate,
    pub priorities: Vec<Priority>,
    pub stream_links: Vec<StreamPriorityLink>,
    pub due: Vec<Todo>,
    pub main: Vec<Todo>,
    pub blocked: Vec<Todo>,
    pub later: Vec<Todo>,
    pub by_priority: bool,
    pub show_later: bool,
}

#[derive(Debug)]
pub struct TodoListView<'a> {
    pub loaded: &'a LoadedTodoStore,
    pub priorities: Vec<Priority>,
    pub stream_links: Vec<StreamPriorityLink>,
    pub todos: Vec<Todo>,
}

impl<'a> TodoView<'a> {
    pub fn from_loaded(
        loaded: &'a LoadedTodoStore,
        today: NaiveDate,
        options: NextOptions,
    ) -> Self {
        let priorities = priority_items(loaded);
        let stream_links = stream_links(loaded);
        let todos = todo_items(loaded);
        let sections = classify_next_sections(&todos, today);
        Self {
            loaded,
            today,
            priorities,
            stream_links,
            due: filtered(sections.due, options.quick, None),
            main: filtered(sections.main, options.quick, options.top),
            blocked: filtered(sections.blocked, options.quick, None),
            later: filtered(sections.later, options.quick, None),
            by_priority: options.by_priority,
            show_later: options.later,
        }
    }
}

impl<'a> TodoListView<'a> {
    pub fn from_loaded(loaded: &'a LoadedTodoStore) -> Self {
        Self {
            loaded,
            priorities: priority_items(loaded),
            stream_links: stream_links(loaded),
            todos: todo_items(loaded),
        }
    }
}

pub fn todo_metadata(
    todo: &Todo,
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
    today: Option<NaiveDate>,
) -> String {
    let id = if todo.id.is_empty() {
        "id:missing".to_string()
    } else {
        format!("id:{}", todo.id)
    };
    let rank = priority_rank(todo, priorities, stream_links)
        .map_or_else(|| "rank:-".to_string(), |value| format!("rank:{value}"));
    let relation = relation_label(todo, priorities, stream_links);
    let due = due_label(todo, today);
    let when = when_label(todo);
    let stream = todo
        .stream
        .as_ref()
        .map_or_else(|| "stream:-".to_string(), |name| format!("stream:{name}"));
    let quick = if todo.quick { " quick" } else { "" };
    let pin = if todo.pin { " pin" } else { "" };
    let block = block_label(todo);
    format!("[{id} {rank} {relation} {stream}{due}{when}{quick}{pin}{block}]")
}

pub struct PriorityGroup<'a> {
    pub label: String,
    pub todos: Vec<&'a Todo>,
    sort_value: Option<i32>,
    sort_slug: String,
}

pub fn priority_groups<'a>(
    todos: &'a [Todo],
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
) -> Vec<PriorityGroup<'a>> {
    let mut groups: Vec<PriorityGroup<'a>> = Vec::new();
    for todo in todos {
        let best_priority = best_priority(todo, priorities, stream_links);
        let label = best_priority.map_or_else(
            || "priority:-".to_string(),
            |priority| format!("priority:{}({})", priority.slug, priority.value),
        );
        if let Some(group) = groups.iter_mut().find(|group| group.label == label) {
            group.todos.push(todo);
        } else {
            groups.push(PriorityGroup {
                label,
                todos: vec![todo],
                sort_value: best_priority.map(|priority| priority.value),
                sort_slug: best_priority.map_or_else(String::new, |priority| priority.slug.clone()),
            });
        }
    }
    groups.sort_by(priority_group_order);
    groups
}

fn priority_group_order(left: &PriorityGroup<'_>, right: &PriorityGroup<'_>) -> std::cmp::Ordering {
    match (left.sort_value, right.sort_value) {
        (Some(left_value), Some(right_value)) => right_value
            .cmp(&left_value)
            .then_with(|| left.sort_slug.cmp(&right.sort_slug)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.label.cmp(&right.label),
    }
}

fn filtered(todos: Vec<&Todo>, quick: bool, top: Option<usize>) -> Vec<Todo> {
    let filtered = todos.into_iter().filter(|todo| !quick || todo.quick);
    match top {
        Some(limit) => filtered.take(limit).cloned().collect(),
        None => filtered.cloned().collect(),
    }
}

pub(super) fn priority_items(loaded: &LoadedTodoStore) -> Vec<Priority> {
    loaded
        .store
        .priorities
        .items
        .iter()
        .filter_map(|line| match &line.item {
            PriorityFileItem::Priority(priority) => Some(priority.clone()),
            PriorityFileItem::Raw(_) => None,
        })
        .collect()
}

pub(super) fn stream_links(loaded: &LoadedTodoStore) -> Vec<StreamPriorityLink> {
    loaded
        .store
        .streams
        .items
        .iter()
        .filter_map(|line| match &line.item {
            StreamFileItem::Link(link) => Some(link.clone()),
            StreamFileItem::Raw(_) => None,
        })
        .collect()
}

pub(super) fn todo_items(loaded: &LoadedTodoStore) -> Vec<Todo> {
    loaded
        .store
        .todos
        .items
        .iter()
        .filter_map(|line| match &line.item {
            TodoFileItem::Todo(todo) => Some(todo.clone()),
            TodoFileItem::Raw(raw) => parse_idless_todo(raw),
        })
        .collect()
}

fn relation_label(
    todo: &Todo,
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
) -> String {
    best_priority(todo, priorities, stream_links).map_or_else(
        || "priority:-".to_string(),
        |priority| format!("priority:{}({})", priority.slug, priority.value),
    )
}

fn due_label(todo: &Todo, today: Option<NaiveDate>) -> String {
    let Some(due) = todo.due else {
        return String::new();
    };
    match today {
        Some(today) if due < today => format!(" overdue:{due}"),
        Some(today) if due == today => " due:today".to_string(),
        Some(_) | None => format!(" due:{due}"),
    }
}

fn when_label(todo: &Todo) -> String {
    todo.when
        .map_or_else(String::new, |when| format!(" when:{when}"))
}

fn block_label(todo: &Todo) -> String {
    todo.block
        .as_ref()
        .map_or_else(String::new, |reason| format!(" blocked:{reason:?}"))
}

fn best_priority<'a>(
    todo: &Todo,
    priorities: &'a [Priority],
    stream_links: &[StreamPriorityLink],
) -> Option<&'a Priority> {
    priorities
        .iter()
        .filter(|priority| priority.status == PriorityStatus::Active)
        .filter(|priority| todo_serves_priority(todo, priority.slug.as_str(), stream_links))
        .max_by_key(|priority| priority.value)
}

fn todo_serves_priority(todo: &Todo, slug: &str, stream_links: &[StreamPriorityLink]) -> bool {
    todo.priority.iter().any(|priority| priority == slug)
        || todo.stream.as_ref().is_some_and(|stream| {
            stream_links
                .iter()
                .any(|link| link.stream == *stream && link.priority == slug)
        })
}

fn parse_idless_todo(line: &str) -> Option<Todo> {
    let todo = parse_todo_lenient(line)?;
    if !todo.id.is_empty() {
        return None;
    }
    Some(todo)
}
