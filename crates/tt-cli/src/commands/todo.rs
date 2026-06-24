use anyhow::Result;
use chrono::Local;
use tt_db::Database;

use crate::Config;
use crate::commands::report::Period;
use crate::todo_store::load_read_only;

mod check;
mod drift;
mod ids;
mod json;
mod mutate;
mod order_edit;
mod raw;
mod render;
mod view;

#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "options mirror independent read-only CLI flags"
)]
pub struct NextOptions {
    pub top: Option<usize>,
    pub quick: bool,
    pub json: bool,
    pub by_priority: bool,
    pub later: bool,
}

#[derive(Debug, Clone)]
pub struct AddOptions {
    pub text: String,
    pub priority: Vec<String>,
    pub stream: Option<String>,
    pub due: Option<String>,
    pub when: Option<String>,
    pub quick: bool,
    pub pin: bool,
}

#[derive(Debug, Clone)]
pub struct RankOptions {
    pub id: String,
    pub top: bool,
    pub above: Option<String>,
    pub below: Option<String>,
}

pub fn run_next(config: &Config, options: NextOptions) -> Result<()> {
    let loaded = load_read_only(config)?;
    let today = Local::now().date_naive();
    let view = view::TodoView::from_loaded(&loaded, today, options);
    if options.json {
        print!("{}", json::render_next(&view)?);
    } else {
        print!("{}", render::render_next(&view)?);
    }
    Ok(())
}

pub fn run_ls(config: &Config) -> Result<()> {
    let loaded = load_read_only(config)?;
    let view = view::TodoListView::from_loaded(&loaded);
    print!("{}", render::render_ls(&view)?);
    Ok(())
}

pub fn run_add(config: &Config, options: AddOptions) -> Result<()> {
    mutate::run_add(config, options)
}

pub fn run_done(config: &Config, id: &str) -> Result<()> {
    mutate::run_done(config, id)
}

pub fn run_defer(config: &Config, id: &str, date: &str) -> Result<()> {
    mutate::run_defer(config, id, date)
}

pub fn run_block(config: &Config, id: &str, reason: &str) -> Result<()> {
    mutate::run_block(config, id, reason)
}

pub fn run_unblock(config: &Config, id: &str) -> Result<()> {
    mutate::run_unblock(config, id)
}

pub fn run_rank(config: &Config, options: &RankOptions) -> Result<()> {
    mutate::run_rank(config, options)
}

pub fn run_normalize_ids(config: &Config) -> Result<()> {
    mutate::run_normalize_ids(config)
}

pub fn run_check(config: &Config, json: bool) -> Result<()> {
    let loaded = load_read_only(config)?;
    print!("{}", check::render_check(&loaded, json)?);
    Ok(())
}

pub fn run_drift(db: &Database, config: &Config, period: Period, json: bool) -> Result<()> {
    drift::run(db, config, period, json)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::ids;

    #[test]
    fn mint_todo_id_retries_when_candidate_collides() {
        // Given: the first generated id already exists and the second one does not.
        let existing = HashSet::from(["td_0000000000".to_string()]);
        let byte_batches = [
            [0_u8; 16],
            [
                0x08, 0x86, 0x42, 0x98, 0xe8, 0x4a, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x11, 0x22,
                0x33, 0x44,
            ],
        ];
        let mut index = 0usize;

        // When: an id is minted with a deterministic byte source.
        let id = ids::mint_todo_id_with(&existing, || {
            let bytes = byte_batches[index];
            index += 1;
            bytes
        });

        // Then: the collision is skipped and the next candidate is returned.
        assert_eq!(id, "td_123456789a");
    }
}
