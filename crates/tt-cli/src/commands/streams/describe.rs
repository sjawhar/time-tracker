use anyhow::{Context, Result, bail};
use tt_db::{Database, Stream};
use tt_llm::Classifier;

const MAX_EVIDENCE_ITEMS: usize = 10;

#[derive(Debug, PartialEq, Eq)]
pub struct DescriptionProposal {
    pub stream_ref: String,
    pub description: String,
}

pub fn describe(db: &Database, stream_ref: &str, description: &str) -> Result<()> {
    let Some(stream) = db
        .resolve_stream(stream_ref)
        .context("failed to resolve stream")?
    else {
        bail!("no stream matching '{stream_ref}' (tried id, slug, exact name)");
    };

    db.set_stream_description(&stream.id, description)
        .with_context(|| format!("failed to set description on stream {}", stream.id))?;
    println!("Set description on stream {}", stream.id);
    Ok(())
}

pub fn backfill(
    db: &Database,
    classifier: &dyn Classifier,
    apply: bool,
) -> Result<Vec<DescriptionProposal>> {
    let streams = db.get_streams().context("failed to load streams")?;
    let mut proposals = Vec::new();

    for stream in streams
        .into_iter()
        .filter(|stream| stream.description.is_none())
    {
        let evidence = evidence_for_stream(db, &stream)?;
        let description = classifier
            .describe_stream(&evidence)
            .with_context(|| format!("failed to describe stream {}", stream.id))?;
        let stream_ref = stream.slug.clone().unwrap_or_else(|| stream.id.clone());

        if apply {
            db.set_stream_description(&stream.id, &description)
                .with_context(|| format!("failed to set description on stream {}", stream.id))?;
        }

        println!("{stream_ref}: {description}");
        proposals.push(DescriptionProposal {
            stream_ref,
            description,
        });
    }

    Ok(proposals)
}

fn evidence_for_stream(db: &Database, stream: &Stream) -> Result<String> {
    let events = db
        .get_events_by_stream(&stream.id)
        .with_context(|| format!("failed to load events for stream {}", stream.id))?;
    let mut session_ids = Vec::new();
    let mut window_titles = Vec::new();

    for event in events.iter().rev() {
        if let Some(session_id) = &event.session_id
            && session_ids.len() < MAX_EVIDENCE_ITEMS
            && !session_ids.contains(session_id)
        {
            session_ids.push(session_id.clone());
        }
        if let Some(window_title) = &event.window_title
            && window_titles.len() < MAX_EVIDENCE_ITEMS
            && !window_titles.contains(window_title)
        {
            window_titles.push(window_title.clone());
        }
        if session_ids.len() == MAX_EVIDENCE_ITEMS && window_titles.len() == MAX_EVIDENCE_ITEMS {
            break;
        }
    }

    session_ids.reverse();
    window_titles.reverse();
    let mut starting_prompts = Vec::new();
    for session_id in session_ids {
        if let Some((session, _)) = db
            .get_agent_session(&session_id)
            .with_context(|| format!("failed to load session {session_id}"))?
            && let Some(prompt) = session.starting_prompt
        {
            starting_prompts.push(prompt);
        }
    }

    let prompts = if starting_prompts.is_empty() {
        "(none)".to_owned()
    } else {
        starting_prompts.join("\n")
    };
    let titles = if window_titles.is_empty() {
        "(none)".to_owned()
    } else {
        window_titles.join("\n")
    };

    Ok(format!(
        "Session starting prompts:\n{prompts}\n\nWindow titles:\n{titles}"
    ))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use tt_core::{
        EventType,
        session::{AgentSession, SessionSource, SessionType},
    };
    use tt_db::{Database, StoredEvent, Stream};
    use tt_llm::MockClassifier;

    use super::{backfill, describe, evidence_for_stream};

    fn database_with_stream() -> Database {
        let db = Database::open_in_memory().unwrap();
        let now = Utc::now();
        db.insert_stream(&Stream {
            id: "stream-1".to_owned(),
            name: Some("Planning".to_owned()),
            slug: Some("planning".to_owned()),
            description: None,
            color: None,
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        })
        .unwrap();
        db
    }

    fn add_backfill_evidence(db: &Database) {
        let now = Utc::now();
        db.upsert_agent_session(
            &AgentSession {
                session_id: "session-1".to_owned(),
                source: SessionSource::Claude,
                parent_session_id: None,
                session_type: SessionType::User,
                project_path: "/work/planning".to_owned(),
                project_name: "planning".to_owned(),
                start_time: now,
                end_time: None,
                message_count: 1,
                summary: None,
                user_prompts: vec!["Plan the next release.".to_owned()],
                starting_prompt: Some("Plan the next release.".to_owned()),
                assistant_message_count: 0,
                tool_call_count: 0,
                user_message_timestamps: Vec::new(),
                tool_call_timestamps: Vec::new(),
            },
            None,
        )
        .unwrap();
        db.insert_events(&[StoredEvent {
            id: "event-1".to_owned(),
            timestamp: now,
            event_type: EventType::WindowFocus,
            source: "test".to_owned(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: Some("editor".to_owned()),
            window_title: Some("release-plan.md".to_owned()),
            action: None,
            cwd: Some("/work/planning".to_owned()),
            session_id: Some("session-1".to_owned()),
            stream_id: Some("stream-1".to_owned()),
            assignment_source: Some("test".to_owned()),
            data: json!({}),
        }])
        .unwrap();
    }

    fn mock_classifier(description: &str) -> MockClassifier {
        let classifier = MockClassifier::default();
        classifier
            .descriptions
            .lock()
            .unwrap()
            .push_back(Ok(description.to_owned()));
        classifier
    }

    #[test]
    fn describe_sets_description_when_stream_is_resolved_by_slug() {
        let db = database_with_stream();

        describe(&db, "planning", "Plan the next release.").unwrap();

        let stream = db.get_stream("stream-1").unwrap().unwrap();
        assert_eq!(
            stream.description.as_deref(),
            Some("Plan the next release.")
        );
    }

    #[test]
    fn describe_errors_when_stream_reference_is_unknown() {
        let db = database_with_stream();

        let error = describe(&db, "missing", "Plan the next release.").unwrap_err();

        assert!(error.to_string().contains("no stream matching"));
    }

    #[test]
    fn backfill_returns_proposal_without_writing_when_apply_is_false() {
        let db = database_with_stream();
        add_backfill_evidence(&db);
        let classifier = mock_classifier("Release planning work.");

        let proposals = backfill(&db, &classifier, false).unwrap();

        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].description, "Release planning work.");
        let stream = db.get_stream("stream-1").unwrap().unwrap();
        assert!(stream.description.is_none());
    }

    #[test]
    fn backfill_writes_mock_description_when_apply_is_true() {
        let db = database_with_stream();
        add_backfill_evidence(&db);
        let classifier = mock_classifier("Release planning work.");

        backfill(&db, &classifier, true).unwrap();

        let stream = db.get_stream("stream-1").unwrap().unwrap();
        assert_eq!(
            stream.description.as_deref(),
            Some("Release planning work.")
        );
    }

    #[test]
    fn evidence_includes_session_starting_prompts_and_window_titles() {
        let db = database_with_stream();
        add_backfill_evidence(&db);
        let stream = db.get_stream("stream-1").unwrap().unwrap();

        let evidence = evidence_for_stream(&db, &stream).unwrap();

        assert!(evidence.contains("Plan the next release."));
        assert!(evidence.contains("release-plan.md"));
    }
}
