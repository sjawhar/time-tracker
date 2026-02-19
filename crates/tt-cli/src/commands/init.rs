//! Init command for establishing machine identity.

use anyhow::Result;

use crate::machine;

/// Runs the init command.
pub fn run(label: Option<&str>) -> Result<()> {
    let identity = machine::init_machine(label)?;

    println!("Machine ID: {}", identity.machine_id);
    println!("Label:      {}", identity.label);
    println!("Saved to:   {}", machine::machine_json_path()?.display());

    Ok(())
}
