// allow: SIZE_OK — Task 6 requires proposal behavior and its in-file tests to remain colocated.
use std::fmt::Write;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tt_db::{Database, Proposal, ProposalStatus};

use crate::commands::util::format_age;

#[derive(Deserialize)]
struct NewStreamProposal {
    name: String,
}

/// Marks a proposal whose stream target names no existing stream id.
///
/// Two causes, one consequence: the stream was dissolved after the verdict, or the
/// classifier answered with a slug or a placeholder instead of an id. Acceptance
/// matches on id alone, so neither can ever be accepted. Without the marker a
/// stranded target renders as its own bare uuid, which is exactly how a real but
/// unnamed stream renders — so a proposal nobody can act on reads as work waiting on
/// a decision.
const STRANDED_TARGET_MARKER: &str = "(gone)";

pub fn list(db: &Database) -> Result<()> {
    print!("{}", format_proposals(db, Utc::now())?);
    Ok(())
}

pub fn format_proposals(db: &Database, now: DateTime<Utc>) -> Result<String> {
    let proposals = db
        .pending_proposals_by_attention()
        .context("failed to load pending proposals")?;
    // An empty queue says so, rather than rendering an explainer, column headers and a
    // separator above nothing. Every sibling command does this ("No todos yet.", "No
    // priorities yet.", "No machines registered yet.", "No streams with activity..."), and
    // scaffolding with zero rows reads like a command that failed halfway.
    if proposals.is_empty() {
        return Ok("PROPOSALS\n\nNo proposals pending.\n".to_string());
    }
    let mut output = String::from(
        "PROPOSALS\n\nOrdered by attention resolved: the user_message and focus events each would attribute.\n\nID        Age   Attention  Scope       Proposed stream  Confidence  Reasoning\n--------  ----  ---------  ----------  ---------------  ----------  ---------\n",
    );
    let mut stranded = 0_u32;
    for ranked in proposals {
        let proposal = &ranked.proposal;
        let scope = match (&proposal.session_id, &proposal.event_ids) {
            (Some(_), None) => "1 session".to_string(),
            (None, Some(ids)) => format!("{} events", ids.len()),
            (Some(_), Some(_)) => bail!(
                "proposal {} has both session and event targets",
                proposal.id
            ),
            (None, None) => bail!("proposal {} has no assignment target", proposal.id),
        };
        let (target, answerable) = proposal_target(db, proposal)?;
        if !answerable {
            stranded += 1;
        }
        let id: String = proposal.id.chars().take(8).collect();
        writeln!(
            output,
            "{id:<8}  {:>4}  {:>9}  {scope:<10}  {target:<15}  {:>9.0}%  {}",
            format_age(proposal.created_at, now),
            ranked.attention_events,
            proposal.confidence * 100.0,
            truncate_reasoning(&proposal.reasoning)
        )?;
    }
    if stranded > 0 {
        writeln!(output)?;
        writeln!(
            output,
            "{stranded} marked {STRANDED_TARGET_MARKER} name no existing stream id, so \
             accepting one is impossible."
        )?;
        writeln!(
            output,
            "They block nothing. Reject with a replacement to keep the attribution \
             ('tt proposals reject <id> --stream <ref>'), or bare to leave the events \
             unassigned ('tt proposals reject <id>')."
        )?;
    }
    Ok(output)
}

pub fn accept(db: &Database, id: &str) -> Result<()> {
    let proposal = pending_proposal(db, id)?;
    db.accept_proposal(&proposal.id)
        .context("failed to accept proposal")?;
    Ok(())
}

pub fn reject(db: &Database, id: &str, stream: Option<&str>) -> Result<()> {
    let proposal = pending_proposal(db, id)?;
    if let Some(reference) = stream {
        let destination = db
            .resolve_stream(reference)
            .context("failed to resolve destination stream")?
            .ok_or_else(|| anyhow::anyhow!("stream not found: {reference}"))?;
        assign_user(db, &proposal, &destination.id)?;
        db.mark_streams_for_recompute(&[&destination.id])
            .context("failed to mark destination stream for recomputation")?;
    }
    db.set_proposal_status(&proposal.id, ProposalStatus::Rejected)
        .context("failed to mark proposal rejected")?;
    Ok(())
}

fn pending_proposal(db: &Database, id: &str) -> Result<Proposal> {
    let mut matches = db
        .get_proposals(Some(ProposalStatus::Pending))
        .context("failed to load pending proposals")?
        .into_iter()
        .filter(|proposal| proposal.id == id || proposal.id.starts_with(id));
    let proposal = matches
        .next()
        .ok_or_else(|| anyhow::anyhow!("pending proposal not found: {id}"))?;
    if matches.next().is_some() {
        bail!("proposal ID is ambiguous: {id}");
    }
    Ok(proposal)
}

fn assign_user(db: &Database, proposal: &Proposal, stream_id: &str) -> Result<()> {
    match (&proposal.session_id, &proposal.event_ids) {
        (Some(session_id), None) => db
            .assign_events_by_session_id(session_id, stream_id, "user")
            .context("failed to confirm session assignment")?,
        (None, Some(event_ids)) => db
            .assign_events_by_ids(event_ids, stream_id, "user")
            .context("failed to confirm event assignments")?,
        (Some(_), Some(_)) => bail!(
            "proposal {} has both session and event targets",
            proposal.id
        ),
        (None, None) => bail!("proposal {} has no assignment target", proposal.id),
    };
    Ok(())
}

/// Renders a proposal's stream target, and whether accepting it is still possible.
///
/// A proposal that names an existing stream or mints a new one is answerable. One
/// naming a stream `tt streams dissolve` has since deleted is not, at any confidence,
/// by any reviewer.
fn proposal_target(db: &Database, proposal: &Proposal) -> Result<(String, bool)> {
    match (&proposal.proposed_stream_id, &proposal.proposed_new_stream) {
        (Some(stream_id), None) => {
            let short: String = stream_id.chars().take(8).collect();
            let Some(stream) = db
                .get_stream(stream_id)
                .context("failed to load proposed stream")?
            else {
                return Ok((format!("{STRANDED_TARGET_MARKER} {short}"), false));
            };
            Ok((stream.name.unwrap_or_else(|| stream_id.clone()), true))
        }
        (None, Some(definition)) => {
            let NewStreamProposal { name } =
                serde_json::from_str(definition).context("failed to parse proposed new stream")?;
            Ok((format!("new: {name}"), true))
        }
        (Some(_), Some(_)) => bail!("proposal {} has conflicting stream targets", proposal.id),
        (None, None) => bail!("proposal {} has no proposed stream", proposal.id),
    }
}

fn truncate_reasoning(reasoning: &str) -> String {
    let mut characters = reasoning.chars();
    let prefix: String = characters.by_ref().take(77).collect();
    if characters.next().is_some() {
        format!("{prefix}...")
    } else {
        reasoning.to_string()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;

    fn timestamp() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 24, 9, 0, 0).unwrap()
    }

    fn event(id: &str, session_id: Option<&str>) -> tt_db::StoredEvent {
        tt_db::StoredEvent {
            id: id.to_string(),
            timestamp: timestamp(),
            event_type: tt_core::EventType::AgentToolUse,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: None,
            session_id: session_id.map(str::to_string),
            stream_id: None,
            assignment_source: None,
            data: json!({}),
        }
    }

    fn attention_event(id: &str, session_id: &str) -> tt_db::StoredEvent {
        tt_db::StoredEvent {
            event_type: tt_core::EventType::UserMessage,
            ..event(id, Some(session_id))
        }
    }

    fn stream(id: &str, name: &str) -> tt_db::Stream {
        tt_db::Stream {
            id: id.to_string(),
            name: Some(name.to_string()),
            slug: None,
            description: None,
            color: None,
            created_at: timestamp(),
            updated_at: timestamp(),
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        }
    }

    fn proposal(id: &str) -> tt_db::Proposal {
        tt_db::Proposal {
            id: id.to_string(),
            created_at: timestamp(),
            session_id: None,
            event_ids: None,
            proposed_stream_id: None,
            proposed_new_stream: None,
            confidence: 0.85,
            reasoning: "Matches the active project.".to_string(),
            status: tt_db::ProposalStatus::Pending,
            classifier_generation: None,
        }
    }

    #[test]
    fn accepts_new_stream_when_proposal_defines_session_assignment() {
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_event(&event("event-a", Some("session-a")))
            .unwrap();
        let mut proposed = proposal("proposal-a");
        proposed.session_id = Some("session-a".to_string());
        proposed.proposed_new_stream = Some(
            json!({"name": "Proposal review", "description": "Review classifier proposals", "tags": ["planning", "review"]})
                .to_string(),
        );
        db.insert_proposal(&proposed).unwrap();

        accept(&db, "proposal-a").unwrap();

        let created = db.get_streams().unwrap().pop().unwrap();
        let assigned = db.get_events(None, None).unwrap().pop().unwrap();
        assert_eq!(created.name.as_deref(), Some("Proposal review"));
        assert_eq!(
            created.description.as_deref(),
            Some("Review classifier proposals")
        );
        assert_eq!(db.get_tags(&created.id).unwrap(), ["planning", "review"]);
        assert_eq!(assigned.stream_id.as_deref(), Some(created.id.as_str()));
        assert_eq!(assigned.assignment_source.as_deref(), Some("user"));
        assert_eq!(
            db.get_proposals(None).unwrap()[0].status,
            tt_db::ProposalStatus::Accepted
        );
    }

    #[test]
    fn rejects_without_stream_when_session_proposal_is_declined() {
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_stream(&stream("existing", "Existing stream"))
            .unwrap();
        db.insert_event(&event("event-a", Some("session-a")))
            .unwrap();
        let mut proposed = proposal("proposal-a");
        proposed.session_id = Some("session-a".to_string());
        proposed.proposed_stream_id = Some("existing".to_string());
        db.insert_proposal(&proposed).unwrap();

        reject(&db, "proposal-a", None).unwrap();

        let event = db.get_events(None, None).unwrap().pop().unwrap();
        assert_eq!(event.stream_id, None);
        assert_eq!(event.assignment_source, None);
        assert_eq!(
            db.get_proposals(None).unwrap()[0].status,
            tt_db::ProposalStatus::Rejected
        );
        assert!(db.has_rejected_proposal("session-a", "existing").unwrap());
    }

    #[test]
    fn rejects_with_stream_when_event_proposal_is_redirected() {
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_stream(&stream("destination", "Destination"))
            .unwrap();
        db.insert_event(&event("event-a", None)).unwrap();
        let mut proposed = proposal("proposal-a");
        proposed.event_ids = Some(vec!["event-a".to_string()]);
        proposed.proposed_stream_id = Some("destination".to_string());
        db.insert_proposal(&proposed).unwrap();

        reject(&db, "proposal-a", Some("destination")).unwrap();

        let event = db.get_events(None, None).unwrap().pop().unwrap();
        assert_eq!(event.stream_id.as_deref(), Some("destination"));
        assert_eq!(event.assignment_source.as_deref(), Some("user"));
    }

    #[test]
    fn formats_pending_proposals_for_listing() {
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_stream(&stream("existing", "Existing stream"))
            .unwrap();
        let mut existing = proposal("12345678-existing");
        existing.session_id = Some("session-a".to_string());
        existing.proposed_stream_id = Some("existing".to_string());
        db.insert_proposal(&existing).unwrap();
        let mut new_stream = proposal("abcdefgh-new");
        new_stream.event_ids = Some(vec!["event-a".to_string(), "event-b".to_string()]);
        new_stream.reasoning = "This reasoning deliberately exceeds eighty characters so proposal reviews remain readable in the table."
            .to_string();
        new_stream.proposed_new_stream = Some(
            json!({"name": "New stream", "description": "A new stream", "tags": []}).to_string(),
        );
        db.insert_proposal(&new_stream).unwrap();

        insta::assert_snapshot!(
            format_proposals(&db, timestamp() + chrono::Duration::hours(2)).unwrap()
        );
    }

    #[test]
    fn lists_the_proposal_resolving_the_most_attention_first() {
        // Given: an old proposal resolving one attention event and a newer one resolving
        // three. A queue ordered by age would lead with the older row and spend a
        // reviewer's scarcest resource on the smaller answer.
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_event(&attention_event("event-quiet", "session-quiet"))
            .unwrap();
        for index in 0..3 {
            db.insert_event(&attention_event(
                &format!("event-busy-{index}"),
                "session-busy",
            ))
            .unwrap();
        }
        let mut quiet = proposal("11111111-quiet");
        quiet.session_id = Some("session-quiet".to_string());
        quiet.proposed_new_stream =
            Some(json!({"name": "Quiet work", "description": null, "tags": []}).to_string());
        db.insert_proposal(&quiet).unwrap();
        let mut busy = proposal("22222222-busy");
        busy.created_at = timestamp() + chrono::Duration::hours(1);
        busy.session_id = Some("session-busy".to_string());
        busy.proposed_new_stream =
            Some(json!({"name": "Busy work", "description": null, "tags": []}).to_string());
        db.insert_proposal(&busy).unwrap();

        // When / Then: the younger, heavier proposal leads, and the column says why.
        insta::assert_snapshot!(
            format_proposals(&db, timestamp() + chrono::Duration::hours(2)).unwrap()
        );
    }

    #[test]
    fn a_proposal_naming_a_dissolved_stream_is_not_listed_as_actionable() {
        // Given: one answerable proposal and one whose stream has been dissolved. The
        // dissolved one renders as a bare uuid otherwise, which is how a real but
        // unnamed stream renders too.
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_stream(&stream("existing", "Existing stream"))
            .unwrap();
        let mut answerable = proposal("12345678-existing");
        answerable.session_id = Some("session-a".to_string());
        answerable.proposed_stream_id = Some("existing".to_string());
        db.insert_proposal(&answerable).unwrap();
        let mut stranded = proposal("3542bdf0-c7bf-4d7f-a050-2e1e94fb8abe");
        stranded.session_id = Some("session-b".to_string());
        stranded.proposed_stream_id = Some("0444082a-7c6d-4a40-bd3c-fe5d74960e06".to_string());
        stranded.reasoning = "The most appropriate existing stream is misc: scratch.".to_string();
        db.insert_proposal(&stranded).unwrap();

        // When / Then
        insta::assert_snapshot!(
            format_proposals(&db, timestamp() + chrono::Duration::hours(2)).unwrap()
        );
    }

    #[test]
    fn an_empty_queue_says_so_instead_of_rendering_an_empty_table() {
        // On a fresh install this printed the explainer, the column headers and the
        // separator above zero rows, which reads like a command that failed halfway. Every
        // sibling command has an empty state ("No todos yet.", "No priorities yet.", "No
        // machines registered yet.").
        let db = tt_db::Database::open_in_memory().unwrap();

        let rendered = format_proposals(&db, timestamp()).unwrap();

        assert!(rendered.contains("No proposals pending"), "{rendered}");
        assert!(
            !rendered.contains("Confidence"),
            "no column headers above zero rows: {rendered}"
        );
        assert!(
            !rendered.contains("Ordered by attention"),
            "no ordering explainer when there is nothing ordered: {rendered}"
        );
    }

    #[test]
    fn a_nonempty_queue_still_explains_its_ordering() {
        // The explainer is what tells a reviewer the top of the list is worth the most
        // attribution, so it must survive the empty-state change.
        let db = tt_db::Database::open_in_memory().unwrap();
        db.insert_stream(&stream("stream-a", "agent-c: real work"))
            .unwrap();
        let mut pending = proposal("prop-a");
        pending.proposed_stream_id = Some("stream-a".to_string());
        // A proposal must name what it would assign; format_proposals rejects one that
        // names neither a session nor events, which is a guard worth not tripping here.
        pending.session_id = Some("ses-a".to_string());
        db.insert_proposal(&pending).unwrap();

        let rendered = format_proposals(&db, timestamp()).unwrap();

        assert!(rendered.contains("Ordered by attention"), "{rendered}");
        assert!(rendered.contains("Confidence"), "{rendered}");
        assert!(!rendered.contains("No proposals pending"), "{rendered}");
    }
}
