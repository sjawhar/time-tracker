use anyhow::Context;
use anyhow::Result;
use tt_cli::commands::{priority, report, todo};
use tt_cli::{Config, PriorityAction, TodoAction};
use tt_db::Database;

pub fn run_todo_action(db: Option<&Database>, config: &Config, action: &TodoAction) -> Result<()> {
    match action {
        TodoAction::Next {
            top,
            quick,
            json,
            by_priority,
            later,
        } => todo::run_next(
            config,
            todo::NextOptions {
                top: *top,
                quick: *quick,
                json: *json,
                by_priority: *by_priority,
                later: *later,
            },
        ),
        TodoAction::Ls => todo::run_ls(config),
        TodoAction::Add {
            text,
            priority,
            stream,
            due,
            when,
            quick,
            pin,
        } => todo::run_add(
            config,
            todo::AddOptions {
                text: text.clone(),
                priority: priority.clone(),
                stream: stream.clone(),
                due: due.clone(),
                when: when.clone(),
                quick: *quick,
                pin: *pin,
            },
        ),
        TodoAction::Done { id } => todo::run_done(config, id),
        TodoAction::Defer { id, date } => todo::run_defer(config, id, date),
        TodoAction::Block { id, reason } => todo::run_block(config, id, reason),
        TodoAction::Unblock { id } => todo::run_unblock(config, id),
        TodoAction::Rank {
            id,
            top,
            above,
            below,
        } => todo::run_rank(
            config,
            &todo::RankOptions {
                id: id.clone(),
                top: *top,
                above: above.clone(),
                below: below.clone(),
            },
        ),
        TodoAction::NormalizeIds => todo::run_normalize_ids(config),
        TodoAction::Check { json } => todo::run_check(config, *json),
        TodoAction::Drift {
            week: _,
            last_week,
            day,
            last_day,
            json,
        } => {
            let period = todo_drift_period(*last_week, *day, *last_day);
            let db = db.context("todo drift requires an open database")?;
            todo::run_drift(db, config, period, *json)
        }
    }
}

const fn todo_drift_period(last_week: bool, day: bool, last_day: bool) -> report::Period {
    if last_week {
        report::Period::LastWeek
    } else if day {
        report::Period::Day
    } else if last_day {
        report::Period::LastDay
    } else {
        report::Period::Week
    }
}

pub fn run_priority_action(config: &Config, action: &PriorityAction) -> Result<()> {
    match action {
        PriorityAction::Ls => priority::run_ls(config),
        PriorityAction::Add {
            slug,
            value,
            description,
        } => priority::run_add(
            config,
            priority::AddOptions {
                slug: slug.clone(),
                value: *value,
                description: description.clone(),
            },
        ),
        PriorityAction::Describe { slug, text } => priority::run_describe(config, slug, text),
        PriorityAction::Value { slug, n } => priority::run_value(config, slug, *n),
        PriorityAction::Rename { old_slug, new_slug } => {
            priority::run_rename(config, old_slug, new_slug)
        }
        PriorityAction::Done { slug } => priority::run_done(config, slug),
    }
}
