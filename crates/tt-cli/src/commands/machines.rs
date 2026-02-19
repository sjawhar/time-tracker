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
