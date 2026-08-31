//! What a classifier verdict does to the database.
//!
//! The passes in the parent module decide *which* candidates are looked at; this
//! module decides what each answer means: assign it, propose it for review, junk
//! it, or refuse it.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, warn};
use tt_core::{MisnamedReason, is_misnamed_stream, normalize_stream_name};
use tt_db::JUNK_ASSIGNMENT_SOURCE;
use tt_llm::{
    CLASSIFIER_GENERATION, ClassificationInput, ClassificationOutput, Classifier, LlmError,
    StreamChoice, StreamSummary,
};

use super::AutoClassifyOutcome;
use super::target::AssignmentTarget;
use crate::Config;

/// `assignment_source` recorded when the classifier chose the stream itself.
const INFERRED_ASSIGNMENT_SOURCE: &str = "inferred";

pub(super) struct Resolver<'a> {
    pub(super) db: &'a tt_db::Database,
    pub(super) classifier: &'a dyn Classifier,
    pub(super) roster: Vec<StreamSummary>,
    pub(super) context_instructions: Option<&'a str>,
    confidence_threshold: f64,
    pub(super) outcome: AutoClassifyOutcome,
}

impl<'a> Resolver<'a> {
    pub(super) fn new(
        db: &'a tt_db::Database,
        config: &'a Config,
        classifier: &'a dyn Classifier,
    ) -> Result<Self> {
        // Each stream carries the period it has been active over, because that is what
        // `prompt::build` orders on: it lists only `ROSTER_LIMIT` streams, and a reuse
        // target the model cannot see is a duplicate waiting to be minted. The period
        // comes from `events` rather than `streams.first_event_at`/`last_event_at` because
        // only `tt recompute` writes those and nobody runs it — 758 of the live table's
        // 1,018 streams have `last_event_at` NULL, which would sort three quarters of the
        // roster into an undifferentiated tail. Ordering what the model is *shown* is
        // presentation; the choice stays the model's.
        let windows = db
            .stream_activity_windows()
            .context("load per-stream activity windows for the classifier roster")?;
        let roster = db
            .get_streams_with_tags()
            .context("load stream roster with tags for automatic classification")?
            .into_iter()
            .map(|(stream, tags)| {
                let window = windows.get(&stream.id);
                StreamSummary {
                    slug: stream.slug,
                    id: stream.id,
                    name: stream.name,
                    description: stream.description,
                    tags,
                    first_active: window.map(|window| window.first),
                    last_active: window.map(|window| window.last),
                }
            })
            .collect();
        Ok(Self {
            db,
            classifier,
            roster,
            context_instructions: config.classifier.context_instructions.as_deref(),
            confidence_threshold: config.classifier.confidence_threshold,
            outcome: AutoClassifyOutcome::default(),
        })
    }

    pub(super) fn classify(&mut self, input: &ClassificationInput) -> Option<ClassificationOutput> {
        let result = self
            .classifier
            .classify(input, &self.roster, self.context_instructions);
        self.record_call(&input.session_id, result)
    }

    /// Records one model call's health and hands back whatever verdict it produced.
    ///
    /// Split out of [`Self::classify`] so a call made anywhere else is recorded the same
    /// way. A chunk of calls run concurrently cannot borrow `&mut self` to record itself,
    /// and repeating this match at that call site would put a second writer on a counter
    /// read as *consecutive* failures — one free to drift from this one silently. One
    /// call, one health write, whichever path made it.
    pub(super) fn record_call(
        &mut self,
        session_id: &str,
        result: Result<ClassificationOutput, LlmError>,
    ) -> Option<ClassificationOutput> {
        match result {
            Ok(output) => {
                self.record_success();
                Some(output)
            }
            Err(error) => {
                self.record_failure(session_id, &error);
                None
            }
        }
    }

    /// Clears the failure streak as soon as one call succeeds.
    ///
    /// This is recorded per call because `record_failure` is, and the counter it
    /// resets is read as *consecutive* failures. Deferring it to the end of the pass
    /// made the two asymmetric: a pass is bounded at 200 sessions and runs for hours,
    /// so every failure armed the exponential `classifier_retry_delay` immediately
    /// while the successes that should have cleared it stayed invisible until the pass
    /// finished. A daemon classifying normally therefore reported a day-old
    /// `classifier_last_success_at` beside a rising failure count, and throttled itself
    /// against a provider that was answering. Health writes never bump `db_version`.
    fn record_success(&self) {
        if let Err(record_error) = self.db.record_classifier_success(Utc::now()) {
            warn!(%record_error, "failed to record classifier success");
        }
    }

    /// Counts one failed call and leaves a record of it that names the session.
    ///
    /// Both tallies are bumped here and only here, so the total and the breakdown
    /// cannot drift apart. The per-failure `warn!` is what turns a count back into a
    /// list of sessions to look at, and the cause is what says whether looking is
    /// worth it: a spent quota drains on its own, while output the schema rejects
    /// recurs on every retry.
    fn record_failure(&mut self, session_id: &str, error: &LlmError) {
        self.outcome.errors += 1;
        self.outcome.causes.record(error);
        warn!(session_id, %error, "automatic classification failed");
        if let Err(record_error) = self
            .db
            .record_classifier_failure(Utc::now(), &error.to_string())
        {
            warn!(%record_error, "failed to record classifier failure");
        }
    }

    /// Applies one verdict, returning the stream the target now belongs to.
    ///
    /// `None` means the target is still unassigned — proposed, rejected before, or
    /// refused — which is the honest state for work the classifier could not place.
    pub(super) fn resolve(
        &mut self,
        output: ClassificationOutput,
        target: AssignmentTarget<'_>,
        active_at: Option<DateTime<Utc>>,
    ) -> Result<Option<String>> {
        let ClassificationOutput {
            choice,
            confidence,
            reasoning,
        } = output;
        match choice {
            // Taken at face value: this is the path for sessions that structure could
            // not judge but that are obviously trivial on inspection.
            StreamChoice::Throwaway => self.junk(&target).map(Some),
            // The model was reached, it answered, and its answer named nothing. The
            // candidate is left unassigned, which is the honest state and reads as
            // classification lag.
            StreamChoice::Undetermined => {
                self.decline(&target);
                Ok(None)
            }
            // The model was reached and answered every call, and still never delivered
            // a verdict before its model-call ceiling. Same resting place as a decline,
            // separate tally: this one says a whole turn budget was spent chasing an
            // answer.
            StreamChoice::TurnsExhausted => {
                self.record_spent_turns(&target);
                Ok(None)
            }
            StreamChoice::Existing { stream_id } => {
                // Whitespace inside a uuid carries no meaning, and the model emits it: live
                // refusals included " eb2754ad-e9a3-469b-bdb3-1768bc8860e8" (leading space)
                // and " e638ad6d -fd e6 -4f 85 -b 9d 0-7 e9 b0 2b 98 9a8 " (spaces sprinkled
                // through), both of which name a real stream once the whitespace is removed.
                // Refusing those threw away a correct verdict and left the work unassigned.
                // This mirrors what `normalize_stream_name` already does for names, under the
                // same rule: normalization may only erase differences no human could have
                // meant. It is deliberately only whitespace — a short id, a malformed uuid, or
                // one naming a dissolved stream is still refused, because resolving those
                // would be guessing rather than normalizing.
                let stream_id: String = stream_id.chars().filter(|c| !c.is_whitespace()).collect();
                if !self
                    .db
                    .stream_exists(&stream_id)
                    .context("check that a chosen stream still exists")?
                {
                    self.refuse_missing_stream(&target, &stream_id);
                    return Ok(None);
                }
                if confidence >= self.confidence_threshold {
                    self.assign(&target, &stream_id, INFERRED_ASSIGNMENT_SOURCE)?;
                    return Ok(Some(stream_id));
                }
                if target.was_rejected(self.db, &stream_id)? {
                    self.outcome.skipped += 1;
                } else {
                    self.propose(&target, Some(stream_id), None, confidence, reasoning)?;
                }
                Ok(None)
            }
            StreamChoice::New { name, description } => {
                // Normalized before anything reads it, so the name the guard judges, the
                // name a proposal carries, and the name a row stores are one name. A
                // model emitting a leading space is what minted
                // `" agent-c: eval-3 prometheus test-stage (round 2)"` beside its twin.
                // This only ever tightens `is_misnamed_stream`: collapsing whitespace can
                // reveal a generic shape, never hide one.
                let name = normalize_stream_name(&name);
                if let Some(reason) = is_misnamed_stream(&name) {
                    self.refuse(&target, &name, reason);
                    return Ok(None);
                }
                if target.was_new_stream_rejected(self.db)? {
                    self.outcome.skipped += 1;
                    return Ok(None);
                }
                if confidence >= self.confidence_threshold {
                    let stream_id = self.stream_named(name, description, active_at)?;
                    self.assign(&target, &stream_id, INFERRED_ASSIGNMENT_SOURCE)?;
                    return Ok(Some(stream_id));
                }
                let proposed = serde_json::to_string(&serde_json::json!({
                    "name": name,
                    "description": description,
                    "tags": [],
                }))
                .context("serialize proposed new stream")?;
                self.propose(&target, None, Some(proposed), confidence, reasoning)?;
                Ok(None)
            }
        }
    }

    /// Routes a candidate to the reserved junk stream.
    ///
    /// The events keep their rows, so `tt streams dissolve junk` reverses a rule that
    /// starts eating real work.
    pub(super) fn junk(&mut self, target: &AssignmentTarget<'_>) -> Result<String> {
        let stream_id = self
            .db
            .junk_stream_id()
            .context("resolve the reserved junk stream")?;
        self.assign(target, &stream_id, JUNK_ASSIGNMENT_SOURCE)?;
        self.outcome.junked += 1;
        Ok(stream_id)
    }

    /// Refuses a verdict whose stream name names a posture, a date, or a leftover.
    ///
    /// The candidate is left unassigned on purpose: unassigned reads as classification
    /// lag, while an invented container is a lie that survives. The refusal is logged
    /// and counted so it is visible rather than silent.
    fn refuse(&mut self, target: &AssignmentTarget<'_>, name: &str, reason: MisnamedReason) {
        self.outcome.refused += 1;
        warn!(
            candidate = target.label(),
            proposed_name = name,
            ?reason,
            "refused a stream name that describes no work; leaving the candidate unassigned"
        );
    }

    /// Refuses a verdict naming a stream that no longer exists.
    ///
    /// The roster the classifier answered from is a snapshot, and `tt streams dissolve`
    /// deletes rows behind it, so an id can name nothing by the time the verdict lands.
    /// Writing it raises a foreign-key error that aborts every candidate still queued
    /// behind this one, so the id is refused instead: the candidate stays unassigned,
    /// which reads as classification lag, and the pass carries on.
    ///
    /// Refused rather than substituted, junked or proposed. Junk means "no attributable
    /// work" and this is work whose container vanished; a proposal naming a vanished
    /// stream can never be accepted and would only exile its candidate from later passes.
    fn refuse_missing_stream(&mut self, target: &AssignmentTarget<'_>, stream_id: &str) {
        self.outcome.refused_missing_stream += 1;
        warn!(
            candidate = target.label(),
            missing_stream_id = stream_id,
            "refused a verdict naming a stream that no longer exists; \
             leaving the candidate unassigned"
        );
    }

    /// Records an answer that identified no work.
    ///
    /// Not an error, and counted nowhere near one. The prompt tells the model to
    /// decline rather than invent a container, so a decline is that instruction being
    /// obeyed; treating it as a failed call drove `classifier_consecutive_failures`
    /// and its exponential backoff silenced the daemon's classify loop entirely.
    ///
    /// Not junked either. Junk asserts that no attributable work exists, and nothing
    /// here supports that assertion — the classification simply did not reach one.
    /// Left unassigned, the candidate is re-asked on a later pass, when new events or
    /// a fuller roster may make it answerable.
    ///
    /// Logged at `debug`: unlike a refusal, which is the classifier answering wrongly
    /// and worth a look, this is expected traffic whose volume the pass summary
    /// already reports.
    fn decline(&mut self, target: &AssignmentTarget<'_>) {
        self.outcome.undetermined += 1;
        debug!(
            candidate = target.label(),
            "classifier identified no work; leaving the candidate unassigned"
        );
    }

    /// Records a classification that spent its whole model-call budget without
    /// answering.
    ///
    /// Not an error, for the same reason a decline is not: the provider answered every
    /// call, so arming `classifier_consecutive_failures` here backs the daemon off from
    /// a classifier that is working. It reached `classifier_last_error` as
    /// `PromptError: MaxTurnsError: reached max turns` with four consecutive failures
    /// behind it, and the exponential backoff that followed cut the drain from 1.67 to
    /// 0.17 classifications a minute.
    ///
    /// Logged at `warn` rather than `debug`, unlike a decline. A decline is expected
    /// traffic; this is the agentic fetch loop failing to converge inside its bound,
    /// which is worth someone's attention even though it costs the pass nothing.
    fn record_spent_turns(&mut self, target: &AssignmentTarget<'_>) {
        self.outcome.turns_exhausted += 1;
        warn!(
            candidate = target.label(),
            "classification spent its whole model-call budget without answering; \
             leaving the candidate unassigned"
        );
    }

    /// Gives every dependent session of `parent_session_id` the stream its parent resolved to.
    pub(super) fn inherit(&mut self, parent_session_id: &str, stream_id: &str) -> Result<()> {
        let mut visited = HashSet::new();
        let mut pending = vec![parent_session_id.to_owned()];
        while let Some(parent_id) = pending.pop() {
            if !visited.insert(parent_id.clone()) {
                continue;
            }
            for dependent_id in self
                .db
                .dependent_session_ids_for_parent(&parent_id)
                .context("load dependents of a classified session")?
            {
                self.outcome.assigned += self
                    .db
                    .inherit_stream_for_session(&dependent_id, stream_id)
                    .context("give a dependent session its parent's stream")?;
                pending.push(dependent_id);
            }
        }
        Ok(())
    }

    /// Writes a verdict onto the target's events and retires the question it answers.
    ///
    /// Superseding lives here rather than in the two high-confidence arms because this is
    /// the single funnel every write of a stream passes through, junk included: once the
    /// events carry a stream, the queued proposal asking where they belong has been
    /// answered and there is nothing left for a reviewer to decide. Junk is an answer of
    /// that kind too — it asserts there is no attributable work — and it stays reversible
    /// through `tt streams dissolve junk`, which hands the events back unassigned for a
    /// fresh pass.
    ///
    /// The order is deliberate: assign first, so a proposal is only ever retired once the
    /// answer that replaces it is committed.
    fn assign(
        &mut self,
        target: &AssignmentTarget<'_>,
        stream_id: &str,
        source: &str,
    ) -> Result<()> {
        self.outcome.assigned += target.assign(self.db, stream_id, source)?;
        self.outcome.superseded += target.supersede_pending_proposals(self.db)?;
        Ok(())
    }

    /// Records an answer too weak to apply, for a human to review.
    ///
    /// A target already holding an answerable proposal gets nothing new: the question is
    /// asked and waiting, and a pass that re-asks it every few minutes would queue a
    /// duplicate every time. Selection used to prevent that by refusing to look at the
    /// candidate at all, which also meant a proposal nobody reviewed exiled its session
    /// for good.
    ///
    /// The question is stamped with this classifier before returning, which is what keeps
    /// the *other* half of that correction from turning into its own waste loop. A run
    /// re-asked because its queued answer was stale would otherwise stay stale and be
    /// re-asked on every subsequent pass; stamping spends the re-ask, so a generation
    /// bump costs one pass over the queue instead of an unbounded number. Bookkeeping
    /// only — the proposal keeps its status, stream, confidence and reasoning, so
    /// nothing here is a verdict on the human's behalf.
    fn propose(
        &mut self,
        target: &AssignmentTarget<'_>,
        proposed_stream_id: Option<String>,
        proposed_new_stream: Option<String>,
        confidence: f64,
        reasoning: String,
    ) -> Result<()> {
        if target.has_pending_proposal(self.db)? {
            target.stamp_pending_proposals(self.db, CLASSIFIER_GENERATION)?;
            self.outcome.skipped += 1;
            return Ok(());
        }
        let (session_id, event_ids) = target.proposal_scope();
        // A window run that gained a focus event no longer matches its own pending
        // proposal, so the guard above lets it through and this files a second answer
        // beside the first. Retire the strictly-less-complete ones now: the queue then
        // holds one question per run, answered with the most evidence anyone had. See
        // `Database::supersede_pending_subset_proposals_for_events`.
        if let Some(ids) = event_ids.as_deref() {
            let retired = self
                .db
                .supersede_pending_subset_proposals_for_events(ids)
                .context("retire less-complete proposals for this window run")?;
            self.outcome.superseded += retired;
        }
        let proposal = tt_db::Proposal {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: Utc::now(),
            session_id,
            event_ids,
            proposed_stream_id,
            proposed_new_stream,
            confidence,
            reasoning,
            status: tt_db::ProposalStatus::Pending,
            classifier_generation: Some(CLASSIFIER_GENERATION),
        };
        self.db
            .insert_proposal(&proposal)
            .context("store automatic classification proposal")?;
        self.outcome.proposed += 1;
        Ok(())
    }

    /// Reuses the stream already carrying this name, or creates one.
    ///
    /// `name` arrives already [normalized](normalize_stream_name), and the reuse check
    /// compares normalized forms on both sides. Comparing raw strings is what let one
    /// leading space mint a second row.
    ///
    /// The roster is consulted first only as a cache. **The database is the authority**,
    /// for two reasons that both bit in production. The roster is a snapshot taken when the
    /// pass began and a pass runs for hours, so it goes stale against other writers — two
    /// rows named `agent-c: eval-3 traccar environment (eval-3 integration)` were minted
    /// eleven minutes apart. And the roster the *model* saw is now capped at
    /// `tt_llm::ROSTER_LIMIT`, so a name it proposes may belong to a stream it was never
    /// shown; turning that into reuse is exactly what makes the cap safe to have.
    ///
    /// An exact match is only half of that, and the half that stopped being enough: the
    /// model rarely re-words an unseen stream to the character, and every near miss minted
    /// a row — 1,638 streams in August against 143 in May. So once the exact lookup misses,
    /// [`tt_db::Database::find_stream_by_near_name`] asks the narrower question of the
    /// whole table: is some stream already covering this initiative. Exact still runs
    /// first and still wins, because it is the one answer that is not a judgement.
    ///
    /// A `None` description is stored as one: the stream is real and named, it simply
    /// has no description yet, which is exactly the population
    /// `tt streams describe --backfill` selects.
    fn stream_named(
        &mut self,
        name: String,
        description: Option<String>,
        active_at: Option<DateTime<Utc>>,
    ) -> Result<String> {
        if let Some(stream_id) = self
            .roster
            .iter()
            .find(|stream| {
                stream
                    .name
                    .as_deref()
                    .is_some_and(|listed| normalize_stream_name(listed) == name)
            })
            .map(|stream| stream.id.clone())
        {
            return Ok(stream_id);
        }
        let exact = self
            .db
            .find_stream_by_normalized_name(&name)
            .context("look for a stream already carrying the chosen name")?;
        let existing = match exact {
            Some(found) => Some(found),
            None => self
                .db
                .find_stream_by_near_name(&name)
                .context("look for a stream already covering the chosen name's initiative")?,
        };
        if let Some(existing) = existing {
            // Both names, because a near match is a judgement and this line is what makes
            // it auditable after the fact.
            debug!(
                stream_id = existing.id,
                name,
                reused = existing.name.as_deref().unwrap_or_default(),
                "reusing a stream the roster did not list"
            );
            // Read fresh so the reused stream competes on its real period rather than on
            // the `streams` columns, which only `tt recompute` writes.
            let window = self
                .db
                .stream_activity_window(&existing.id)
                .context("load the activity window of a reused stream")?;
            // Cached so the rest of the pass answers from memory, and so the roster the
            // model reads next carries it too.
            self.roster.push(StreamSummary {
                slug: existing.slug,
                id: existing.id.clone(),
                name: existing.name,
                description: existing.description,
                tags: self
                    .db
                    .get_tags(&existing.id)
                    .context("load tags of a reused stream")?,
                first_active: window.map(|window| window.first),
                last_active: window.map(|window| window.last),
            });
            return Ok(existing.id);
        }
        let now = Utc::now();
        let stream = tt_db::Stream {
            id: uuid::Uuid::new_v4().to_string(),
            name: Some(name),
            slug: None,
            description,
            color: None,
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: true,
        };
        let id = stream.id.clone();
        self.db
            .insert_stream(&stream)
            .context("create automatically classified stream")?;
        self.roster.push(StreamSummary {
            slug: stream.slug,
            id: id.clone(),
            name: stream.name,
            description: stream.description,
            tags: Vec::new(),
            // The period of the work this stream was just minted for. Leaving it `None`
            // made the stream invisible for the rest of the pass and manufactured
            // near-duplicates: `proximity` sorts a stream with no period behind every
            // stream that has one, and `ROSTER_LIMIT` then cuts the tail, so the model
            // could not see the stream it had just created and named a near-miss for the
            // same work -- which `find_stream_by_normalized_name` cannot collapse,
            // precisely because it is not an exact match. Three streams for one
            // initiative were minted this way inside seven hours.
            //
            // This invents nothing: the session being classified *is* the stream's first
            // activity, so this is what `stream_activity_windows` reports for it on the
            // next pass, one pass earlier.
            first_active: active_at,
            last_active: active_at,
        });
        Ok(id)
    }
}
