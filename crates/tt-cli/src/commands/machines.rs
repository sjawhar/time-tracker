//! Machines command for listing known remotes.

use anyhow::Result;
use tt_db::Database;

/// Runs the machines command.
pub fn run(db: &Database) -> Result<()> {
    let machines = db.list_machines()?;

    if machines.is_empty() {
        println!("No machines registered yet. Run 'tt sync <remote>' to import from a remote.");
        return Ok(());
    }

    println!("{:<38} {:<20} LAST SYNC", "MACHINE ID", "LABEL");
    for machine in &machines {
        let last_sync = machine.last_sync_at.as_deref().unwrap_or("never");
        println!(
            "{:<38} {:<20} {}",
            machine.machine_id, machine.label, last_sync
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format_machines_output(db: &Database) -> Result<String> {
        let machines = db.list_machines()?;
        let mut output = String::new();
        if machines.is_empty() {
            output.push_str(
                "No machines registered yet. Run 'tt sync <remote>' to import from a remote.\n",
            );
        } else {
            use std::fmt::Write;
            writeln!(output, "{:<38} {:<20} LAST SYNC", "MACHINE ID", "LABEL").unwrap();
            for machine in &machines {
                let last_sync = machine.last_sync_at.as_deref().unwrap_or("never");
                writeln!(
                    output,
                    "{:<38} {:<20} {}",
                    machine.machine_id, machine.label, last_sync
                )
                .unwrap();
            }
        }
        Ok(output)
    }

    #[test]
    fn test_machines_empty() {
        let db = Database::open_in_memory().unwrap();
        let output = format_machines_output(&db).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_machines_with_entries() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_machine(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "devbox",
            Some("last-event-1"),
        )
        .unwrap();
        db.upsert_machine("11111111-2222-3333-4444-555555555555", "gpu-server", None)
            .unwrap();
        let output = format_machines_output(&db).unwrap();
        // Replace timestamps with fixed value for snapshot stability
        let output = regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z")
            .unwrap()
            .replace_all(&output, "2025-01-01T00:00:00.000Z")
            .to_string();
        insta::assert_snapshot!(output);
    }
}
