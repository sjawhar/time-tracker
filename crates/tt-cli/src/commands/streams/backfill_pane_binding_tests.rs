use super::backfill_pane_session_bindings;
use tt_db::{Database, ReleaseMode};

#[test]
fn pane_binding_backfill_dry_run_writes_nothing() {
    let db = Database::open_in_memory().unwrap();
    let version_before = db.get_db_version().unwrap();

    backfill_pane_session_bindings(&db, ReleaseMode::DryRun).unwrap();

    assert_eq!(db.get_db_version().unwrap(), version_before);
}
