use tt_core::todos::{
    FileLine, LineEnding, Priority, PriorityFileItem, StreamFileItem, StreamPriorityLink, Todo,
    TodoFileItem, priority_rank,
};

use crate::todo_store::LoadedTodoStore;

pub fn insert_todo_by_rank(loaded: &mut LoadedTodoStore, todo: Todo) {
    let priorities = priority_items(loaded);
    let stream_links = stream_links(loaded);
    let rank = priority_rank(&todo, &priorities, &stream_links);
    let new_line = FileLine {
        item: TodoFileItem::Todo(todo),
        line_ending: LineEnding::Lf,
    };
    let pinned = pinned_lines(loaded);
    let mut non_pinned = non_pinned_lines(loaded);
    let insert_at = non_pinned_insert_index(&non_pinned, rank, &priorities, &stream_links);
    non_pinned.insert(insert_at, new_line);
    loaded.store.todos.items =
        project_with_pins(pinned, non_pinned, loaded.store.todos.items.len() + 1);
}

fn non_pinned_insert_index(
    lines: &[FileLine<TodoFileItem>],
    rank: Option<i32>,
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
) -> usize {
    lines
        .iter()
        .position(|line| match &line.item {
            TodoFileItem::Todo(todo) => {
                rank_is_strictly_lower(priority_rank(todo, priorities, stream_links), rank)
            }
            TodoFileItem::Raw(_) => false,
        })
        .unwrap_or(lines.len())
}

fn project_with_pins(
    pinned: Vec<(usize, FileLine<TodoFileItem>)>,
    non_pinned: Vec<FileLine<TodoFileItem>>,
    new_len: usize,
) -> Vec<FileLine<TodoFileItem>> {
    let mut pinned_iter = pinned.into_iter().peekable();
    let mut non_pinned_iter = non_pinned.into_iter();
    let mut projected = Vec::with_capacity(new_len);
    for index in 0..new_len {
        if pinned_iter
            .peek()
            .is_some_and(|(pinned_index, _)| *pinned_index == index)
        {
            if let Some((_, line)) = pinned_iter.next() {
                projected.push(line);
            }
        } else if let Some(line) = non_pinned_iter.next() {
            projected.push(line);
        }
    }
    projected.extend(non_pinned_iter);
    projected
}

fn pinned_lines(loaded: &LoadedTodoStore) -> Vec<(usize, FileLine<TodoFileItem>)> {
    loaded
        .store
        .todos
        .items
        .iter()
        .cloned()
        .enumerate()
        .filter(|(_, line)| matches!(&line.item, TodoFileItem::Todo(todo) if todo.pin))
        .collect()
}

fn non_pinned_lines(loaded: &LoadedTodoStore) -> Vec<FileLine<TodoFileItem>> {
    loaded
        .store
        .todos
        .items
        .iter()
        .filter(|line| !matches!(&line.item, TodoFileItem::Todo(todo) if todo.pin))
        .cloned()
        .collect()
}

const fn rank_is_strictly_lower(existing: Option<i32>, new_rank: Option<i32>) -> bool {
    match (existing, new_rank) {
        (Some(existing), Some(new_rank)) => existing < new_rank,
        (None, Some(_)) => true,
        (Some(_) | None, None) => false,
    }
}

fn priority_items(loaded: &LoadedTodoStore) -> Vec<Priority> {
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

fn stream_links(loaded: &LoadedTodoStore) -> Vec<StreamPriorityLink> {
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
