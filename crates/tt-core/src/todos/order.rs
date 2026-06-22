use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;

use super::model::{Priority, PriorityStatus, StreamPriorityLink, Todo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlignmentFinding {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextSection {
    Due,
    Main,
    Later,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextSections<'a> {
    pub due: Vec<&'a Todo>,
    pub main: Vec<&'a Todo>,
    pub later: Vec<&'a Todo>,
    pub done: Vec<&'a Todo>,
}

#[must_use]
pub fn priority_rank(
    todo: &Todo,
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
) -> Option<i32> {
    let active_values = active_priority_values(priorities);
    todo_priority_slugs(todo, stream_links)
        .filter_map(|slug| active_values.get(slug).copied())
        .max()
}

#[must_use]
pub fn find_alignment(
    todos: &[Todo],
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
) -> Vec<AlignmentFinding> {
    let mut findings = Vec::new();
    findings.extend(duplicate_stream_link_findings(stream_links));
    let mut previous_rank = None;
    let mut has_previous = false;

    for (index, todo) in todos.iter().enumerate() {
        let rank = priority_rank(todo, priorities, stream_links);
        if todo.pin {
            findings.push(AlignmentFinding::Pinned {
                index,
                todo_id: todo.id.clone(),
                rank,
            });
            if has_link_references(todo) && rank.is_none() {
                findings.push(AlignmentFinding::OrphanedLinks {
                    index,
                    todo_id: todo.id.clone(),
                });
            }
            continue;
        }
        if has_link_references(todo) && rank.is_none() {
            findings.push(AlignmentFinding::OrphanedLinks {
                index,
                todo_id: todo.id.clone(),
            });
        }
        if has_previous && rank_is_greater(rank, previous_rank) {
            findings.push(AlignmentFinding::Misordered {
                index,
                todo_id: todo.id.clone(),
                rank,
                previous_rank,
            });
        }
        previous_rank = rank;
        has_previous = true;
    }

    findings
}

fn duplicate_stream_link_findings(stream_links: &[StreamPriorityLink]) -> Vec<AlignmentFinding> {
    let mut priorities_by_stream: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for link in stream_links {
        priorities_by_stream
            .entry(link.stream.as_str())
            .or_default()
            .push(link.priority.as_str());
    }
    priorities_by_stream
        .into_iter()
        .filter(|(_, priorities)| priorities.len() > 1)
        .map(
            |(stream, priorities)| AlignmentFinding::DuplicateStreamLink {
                stream: stream.to_string(),
                priorities: priorities
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            },
        )
        .collect()
}

#[must_use]
pub fn classify_next_sections(todos: &[Todo], today: NaiveDate) -> NextSections<'_> {
    let mut sections = NextSections {
        due: Vec::new(),
        main: Vec::new(),
        later: Vec::new(),
        done: Vec::new(),
    };
    for todo in todos {
        match next_section(todo, today) {
            NextSection::Due => sections.due.push(todo),
            NextSection::Main => sections.main.push(todo),
            NextSection::Later => sections.later.push(todo),
            NextSection::Done => sections.done.push(todo),
        }
    }
    sections
}

impl<'a> NextSections<'a> {
    #[must_use]
    pub fn get(&self, section: NextSection) -> &[&'a Todo] {
        match section {
            NextSection::Due => &self.due,
            NextSection::Main => &self.main,
            NextSection::Later => &self.later,
            NextSection::Done => &self.done,
        }
    }
}

fn active_priority_values(priorities: &[Priority]) -> HashMap<&str, i32> {
    priorities
        .iter()
        .filter(|priority| priority.status == PriorityStatus::Active)
        .map(|priority| (priority.slug.as_str(), priority.value))
        .collect()
}

fn todo_priority_slugs<'a>(
    todo: &'a Todo,
    stream_links: &'a [StreamPriorityLink],
) -> impl Iterator<Item = &'a str> {
    todo.priority
        .iter()
        .map(String::as_str)
        .chain(todo_stream_priority(todo, stream_links))
}

fn todo_stream_priority<'a>(
    todo: &'a Todo,
    stream_links: &'a [StreamPriorityLink],
) -> Option<&'a str> {
    let stream_name = todo.stream.as_deref()?;
    stream_links
        .iter()
        .find(|link| link.stream == stream_name)
        .map(|link| link.priority.as_str())
}

fn has_link_references(todo: &Todo) -> bool {
    !todo.priority.is_empty() || todo.stream.is_some()
}

fn next_section(todo: &Todo, today: NaiveDate) -> NextSection {
    if todo.done {
        return NextSection::Done;
    }
    if todo.due.is_some_and(|due| due <= today) {
        return NextSection::Due;
    }
    if todo.when.is_some_and(|when| when > today) {
        return NextSection::Later;
    }
    NextSection::Main
}

const fn rank_is_greater(current: Option<i32>, previous: Option<i32>) -> bool {
    match (current, previous) {
        (Some(current), Some(previous)) => current > previous,
        (Some(_), None) => true,
        (None, Some(_) | None) => false,
    }
}
