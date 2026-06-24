use std::fmt::Write;

use anyhow::{Context, Result, bail};
use tt_core::todos::{FileLine, LineEnding, Priority, PriorityFileItem, PriorityStatus};

use crate::Config;
use crate::todo_store::{
    LoadedTodoStore, StoreFile, load_mutating, load_read_only, write_priorities,
};

mod rename;
pub use rename::run_rename;

#[derive(Debug, Clone)]
pub struct AddOptions {
    pub value: i32,
    pub slug: String,
    pub description: Option<String>,
}

pub fn run_ls(config: &Config) -> Result<()> {
    let loaded = load_read_only(config)?;
    print!("{}", render_ls(&loaded)?);
    Ok(())
}

pub fn run_add(config: &Config, options: AddOptions) -> Result<()> {
    let AddOptions {
        value,
        slug,
        description,
    } = options;
    let slug = validate_explicit_slug(&slug)?;
    let mut loaded = load_mutating(config)?;
    if priority_index(&loaded, &slug).is_some() {
        bail!("priority '{slug}' already exists");
    }
    loaded.store.priorities.items.push(FileLine {
        item: PriorityFileItem::Priority(Priority {
            slug: slug.clone(),
            value,
            status: PriorityStatus::Active,
            description: description.as_deref().and_then(normalize_description),
        }),
        line_ending: LineEnding::Lf,
    });
    write_priorities(config, &loaded.store.priorities)?;
    println!("{slug}");
    Ok(())
}

pub fn run_value(config: &Config, slug: &str, value: i32) -> Result<()> {
    let mut loaded = load_mutating(config)?;
    let index =
        priority_index(&loaded, slug).with_context(|| format!("priority '{slug}' not found"))?;
    let PriorityFileItem::Priority(priority) = &mut loaded.store.priorities.items[index].item
    else {
        bail!("priority '{slug}' not found");
    };
    priority.value = value;
    write_priorities(config, &loaded.store.priorities)
}

pub fn run_done(config: &Config, slug: &str) -> Result<()> {
    let mut loaded = load_mutating(config)?;
    let index =
        priority_index(&loaded, slug).with_context(|| format!("priority '{slug}' not found"))?;
    let PriorityFileItem::Priority(priority) = &mut loaded.store.priorities.items[index].item
    else {
        bail!("priority '{slug}' not found");
    };
    priority.status = PriorityStatus::Done;
    write_priorities(config, &loaded.store.priorities)
}

pub fn run_describe(config: &Config, slug: &str, text: &str) -> Result<()> {
    let mut loaded = load_mutating(config)?;
    let index =
        priority_index(&loaded, slug).with_context(|| format!("priority '{slug}' not found"))?;
    let PriorityFileItem::Priority(priority) = &mut loaded.store.priorities.items[index].item
    else {
        bail!("priority '{slug}' not found");
    };
    priority.description = normalize_description(text);
    write_priorities(config, &loaded.store.priorities)
}

/// Trim a description; treat empty-after-trim as "no description".
fn normalize_description(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn render_ls(loaded: &LoadedTodoStore) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "PRIORITIES").context("failed to format priority header")?;

    let mut count = 0usize;
    for line in &loaded.store.priorities.items {
        if let PriorityFileItem::Priority(priority) = &line.item {
            count += 1;
            let row = match &priority.description {
                Some(description) => writeln!(
                    output,
                    "- {} — {} value={} status={}",
                    priority.slug,
                    description,
                    priority.value,
                    status_label(priority.status)
                ),
                None => writeln!(
                    output,
                    "- {} value={} status={}",
                    priority.slug,
                    priority.value,
                    status_label(priority.status)
                ),
            };
            row.context("failed to format priority row")?;
        }
    }

    if count == 0 {
        writeln!(output, "No priorities yet.").context("failed to format empty priority list")?;
    }

    let priority_diagnostics = loaded
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.file == StoreFile::Priorities)
        .collect::<Vec<_>>();
    if !priority_diagnostics.is_empty() {
        writeln!(output).context("failed to format priority diagnostics spacer")?;
        writeln!(output, "DIAGNOSTICS").context("failed to format priority diagnostics header")?;
        for diagnostic in priority_diagnostics {
            writeln!(
                output,
                "- {} line {}: {}",
                diagnostic.file.label(),
                diagnostic.diagnostic.line_number,
                diagnostic.diagnostic.reason
            )
            .context("failed to format priority diagnostic")?;
        }
    }

    Ok(output)
}

const fn status_label(status: PriorityStatus) -> &'static str {
    match status {
        PriorityStatus::Active => "active",
        PriorityStatus::Done => "done",
        PriorityStatus::Dropped => "dropped",
    }
}

fn priority_index(loaded: &LoadedTodoStore, slug: &str) -> Option<usize> {
    loaded.store.priorities.items.iter().position(
        |line| matches!(&line.item, PriorityFileItem::Priority(priority) if priority.slug == slug),
    )
}

fn validate_explicit_slug(slug: &str) -> Result<String> {
    if slug.is_empty() {
        bail!("priority slug must not be empty");
    }
    if !slug
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        bail!("priority slug must contain only lowercase ASCII letters, digits, or '-'");
    }
    if !slug
        .chars()
        .any(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
    {
        bail!("priority slug must contain at least one lowercase ASCII letter or digit");
    }
    Ok(slug.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo_store::parse_store_contents;

    #[test]
    fn priority_ls_snapshots() {
        let loaded = parse_store_contents(
            "- [ ] ipi <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\",\"description\":\"IPI launch\"} -->\n- [ ] admin <!-- tt-priority:{\"slug\":\"admin\",\"value\":1,\"status\":\"done\"} -->\n",
            "",
            "",
        );

        insta::assert_snapshot!(render_ls(&loaded).unwrap());
    }

    #[test]
    fn validate_explicit_slug_accepts_valid_and_rejects_empty_or_non_alphanumeric() {
        assert!(super::validate_explicit_slug("ipi").is_ok());
        assert!(super::validate_explicit_slug("a-b").is_ok());
        assert!(super::validate_explicit_slug("dpi-report").is_ok());
        assert!(super::validate_explicit_slug("").is_err());
        assert!(super::validate_explicit_slug("-").is_err());
        assert!(super::validate_explicit_slug("Admin").is_err());
    }
}
