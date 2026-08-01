//! Automatic classification: which candidates a pass spends LLM calls on.
//!
//! Three rules keep `tt classify --auto` safe to leave running:
//!
//! - **Recency first, bounded pass.** Candidates arrive newest-first and each pass is
//!   capped, so today's work is reached before a backlog that takes hours to drain.
//! - **Subagents never reach the LLM.** A subagent serves its parent's task and has
//!   no work stream of its own, so it inherits the stream its parent resolved to.
//! - **Junk before asking.** A session that ran no tool and held at most one exchange
//!   cannot carry work, so it is routed to the reserved junk stream without a call.
//!
//! A fourth rule keeps it *diagnosable*: a failed call is counted against the cause
//! its error names, and the summary reports the split. A pass once printed
//! `errors=315` and nothing else, so a tenth of the backlog failed without leaving a
//! way to tell a spent quota — which drains on its own — from output the schema
//! rejects, which will fail again on every retry.
//!
//! The same rule draws a line the tally must not blur: a model that answered and could
//! not identify the work has not failed, so it lands in `undetermined` rather than in
//! `errors`. Counting it as a failure drove `classifier_consecutive_failures` into an
//! exponential backoff that stopped the daemon's classify loop outright.
//!
//! What each answer *means* lives in [`resolver`].

use std::any::Any;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tt_core::is_structurally_junk;
use tt_db::JUNK_ASSIGNMENT_SOURCE;
use tt_llm::{
    CLASSIFIER_GENERATION, ClassificationInput, ClassificationOutput, Classifier, LlmError,
};

use crate::Config;
use resolver::Resolver;
use target::AssignmentTarget;
use window_runs::{build_unassigned_window_runs, newest_first};

mod resolver;
pub mod session_detail;
mod target;
#[cfg(test)]
mod tests;
mod window_runs;

/// Sessions one pass may look at.
///
/// The bound is what makes recency ordering worth anything: the next pass re-sorts,
/// so a session started since the last pass jumps ahead of a backlog measured in
/// thousands. Raising it classifies nothing faster — the LLM is the bottleneck at
/// roughly ten calls a minute — it only delays the moment today's work is reachable.
const SESSIONS_PER_PASS: usize = 200;

/// Structurally-junk sessions one pass may route before it selects candidates.
///
/// Twenty-five times [`SESSIONS_PER_PASS`], and the asymmetry is the point rather than an
/// oversight. That bound rations **model calls**, each 15 to 90 seconds of waiting on a
/// provider. Routing junk spends no call at all: it is three `UPDATE`s against indexed
/// columns and a decision structure already settled. There is nothing here to ration.
///
/// So the only failure this bound exists to prevent is one pass holding a write
/// transaction open for minutes, which would stall the ingest and sync loops writing to
/// the same database. It is a latency ceiling on a transaction, not a budget.
///
/// **It has to be large to do its job at all.** The whole reason this step exists is that
/// junk was eating a bounded budget from inside: `is_structurally_junk` was checked after
/// selection, so a junk session took one of the 200 session slots despite costing no
/// call. Measured on the live database over one hour, **840 structurally-junk sessions
/// were classified against 20 real ones**, and pass summaries read `junked=171..177` of
/// 200. A bound anywhere near `SESSIONS_PER_PASS` would reproduce exactly that starvation
/// one layer down and leave the backlog draining at today's rate.
const JUNK_ROUTING_PER_PASS: usize = 5000;

/// Session classifications one pass runs at the same time.
///
/// A model call is 15 to 90 seconds of waiting on Anthropic, of which this process
/// spends microseconds — the work is I/O, not computation. Run one at a time, a pass
/// costs the *sum* of its waits: a day's ~30 sessions took minutes of wall clock to
/// answer questions that were all answerable at once. Eight in flight costs the longest
/// wait in each chunk instead.
///
/// Eight rather than the whole 200-session bound, because this is also the number of
/// requests one pass points at the provider at once. Every one of them competes for the
/// same rate limit, and a 200-wide burst turns a healthy pass into the 429s and 529s
/// that `HttpFailure::is_transient` exists to survive — spending the retry allowance on
/// congestion this process created.
///
/// **No guard depends on this number.** Every database write stays on the one connection
/// the resolver holds and stays serial, so changing it changes how long a pass takes and
/// nothing at all about what a pass decides. [`Resolver::classify_sessions`] is where
/// that separation is drawn.
/// Eight, and **sixteen was tried, measured and reverted** — do not raise this again
/// without repeating that measurement. Two 15-minute live windows on the same backlog:
/// eight drained **6.8 real sessions a minute** with **0** rate-limit (429) responses,
/// **0** overloaded (529) responses and **0** classifier errors; sixteen drained **8.0**,
/// an **18%** gain, and produced **2** 429s and **6** 529s. The provider is not the
/// constraint at eight and is at sixteen, so the second half of the width buys almost
/// nothing and starts spending the retry allowance on congestion this process created.
///
/// The cost lands off this process, too, which is why the trade is worse than 18% looks.
/// The key is shared with whatever interactive agents run against the same account, and a
/// pass is hours long, so a burst wide enough to earn 429s spends *someone else's*
/// request budget — for a backlog that drains unattended either way.
const CLASSIFY_CONCURRENCY: usize = 8;

/// Window-focus runs one pass may look at.
///
/// The other half of the same guarantee `SESSIONS_PER_PASS` gives: two bounds rather
/// than one shared budget, so neither phase can spend the other's calls and every pass
/// reaches both. This phase used to have no bound at all, which failed in both
/// directions at once — it could emit a call per run in one unbounded burst, and until
/// it did, its work sat behind a session backlog measured in thousands.
///
/// The number is set by the backlog it has to clear, not merely by the inflow it has to
/// keep pace with. That distinction is the whole point, and getting it wrong cost this
/// bound its first sizing.
///
/// **Pace was the old rule and it was insufficient.** Focus arrives at **1.31 events a
/// minute** at peak (1,882 on 2026-08-06, the heaviest measured day) and the classifier
/// drains at a measured **3.9 classifications a minute** when healthy, so a pass of
/// `SESSIONS_PER_PASS + W` calls takes `(200 + W) / 3.9` minutes and merely keeping pace
/// needs `W >= 1.31 * (200 + W) / 3.9`, i.e. `W >= 100.81`. The original 101 satisfied
/// exactly that and nothing more.
///
/// **Pace is not catch-up, and there was a backlog.** Measured on the live database:
/// **71,714 unassigned focus events across 149 days, ~28,717 runs**, while the whole
/// current week sat at 32h46m of 68h35m direct time unassigned (**48%**) — of which
/// **97.6%** was focus events, because they carry no session and nothing but this phase
/// reaches them. Over a clean 15-minute window 16 arrived and 25 were attributed: a net
/// drain of 0.6 a minute, or **83 days** to clear. A dashboard that is half-blank on
/// today for three months is the exact failure this whole area exists to remove, and
/// direct time is the number the product is asked for.
///
/// **The bound sets a share, and the share is the constraint.** The previous note here
/// claimed raising it "buys nothing the daemon's drain does not already buy" because
/// `should_drain` re-arms a pass that advanced, so passes run back to back. That is
/// false for a backlog: within every pass the phases split calls `200 : W`, so focus
/// receives a *fixed fraction* of all calls no matter how many passes run — 101 of 301
/// is 33.6%, forever. More passes cannot raise a fixed share; only W can.
///
/// **101, and the earlier 400 was sized on the wrong metric.** 400 came from the two
/// backlogs' ratio -- ~28,717 runs against ~3,709 sessions -- which would be right if a
/// call in each phase bought the same thing. It does not, by four orders of magnitude.
/// A session classification attributes every event carrying its id: **~376 events per
/// call**, 1,339,657 across 3,559 sessions. A window-run call attributes **~0.03**, since
/// this phase places ~11 focus events a day against its whole budget and 0 of 403 pending
/// proposals reach the 0.80 threshold.
///
/// So a 600-call pass split 200:400 spends two thirds of itself on the phase that
/// attributes almost nothing. Measured live at that split: `classified_sessions` sat at
/// 2,194 for 25 minutes while the classifier reported success throughout -- the session
/// phase had finished its 200 and the pass was working through 400 window runs, so
/// sessions advance during only a third of each pass. Returning to 101 gives them two
/// thirds, roughly halving the time to clear the session backlog.
///
/// Nothing is starved by this: the bounds remain independent, so focus attribution still
/// receives its own calls every pass regardless of the session backlog, which is the
/// guarantee `window_focus_is_classified_even_when_the_session_backlog_fills_a_whole_pass`
/// pins. 101 is the original pace-based sizing: focus arrives at 1.31 events a minute at
/// peak against a drain of 3.9 classifications a minute, so `W >= 1.31 * (200 + W) / 3.9`
/// gives `W >= 100.81`. Keeping pace is the most this phase can usefully do while its
/// answers do not clear the confidence bar; the backlog it cannot clear is a question of
/// evidence per run, not of calls per pass.
const WINDOW_RUNS_PER_PASS: usize = 101;

/// Observable result of an automatic classification pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AutoClassifyOutcome {
    /// Events that gained a stream.
    pub assigned: u64,
    /// Answers recorded for human review instead of applied.
    pub proposed: u64,
    /// Queued answers a later verdict answered past, retired without a reviewer.
    ///
    /// Its own field because it is the only count that says the review queue shrank
    /// without a human touching it. A proposal escalates a question; it must not also
    /// retire the machine's ability to answer that question later, so a pending one no
    /// longer holds its candidate back — and when a pass does answer confidently, this is
    /// what says the queue is smaller because the work is done rather than lost.
    pub superseded: u64,
    /// Candidates a model call was deliberately not spent on, this classifier having
    /// already answered them.
    ///
    /// Held apart from `skipped`, which counts answers that *cost* a call and then added
    /// nothing to the queue. This one costs nothing, and the split is what makes the
    /// difference visible: a pass whose whole window-run budget went here moves no other
    /// counter at all and would otherwise read as an idle pass rather than a conserved
    /// budget.
    pub skipped_answered: u64,
    /// Candidates left untouched, having been answered or rejected before.
    pub skipped: u64,
    /// Classifier calls that failed.
    pub errors: u64,
    /// Candidates routed to the reserved junk stream.
    pub junked: u64,
    /// Answers refused because the stream name they proposed describes no work.
    pub refused: u64,
    /// Answers refused because the stream they named no longer exists.
    ///
    /// Counted apart from `refused` because the two say different things: a name
    /// describing no work is the classifier inventing a container, while a vanished
    /// id is the classifier answering from a roster that dissolution has since edited.
    pub refused_missing_stream: u64,
    /// Candidates the classifier answered about and declined to place.
    ///
    /// Its own bucket, and deliberately not part of `errors`: the model was reached, it
    /// answered, and its answer was that it could not identify the work — which is the
    /// prompt's own instruction being obeyed rather than a failure to obey it. Counted
    /// as an error it drove `classifier_consecutive_failures`, whose exponential
    /// backoff throttled the daemon's classify loop to silence. Counted apart from
    /// `refused` because a refusal is the classifier answering wrongly and being
    /// overruled, while this is it declining to answer at all.
    pub undetermined: u64,
    /// Candidates whose classification burned every model call it was allowed without
    /// delivering an answer.
    ///
    /// Its own bucket for the same reason `undetermined` is not part of `errors`: no
    /// call failed, so nothing here should arm the failure backoff. Held apart from
    /// `undetermined` because the two ask different things of whoever reads the line. A
    /// declined answer is the prompt working on input it cannot place and asks for
    /// nothing; this says the agentic fetch loop did not converge inside its turn bound,
    /// which is a bound or a prompt to look at. Counted as an error it read as
    /// `PromptError: MaxTurnsError` on `classifier_last_error` and drove the drain down
    /// to 0.17 classifications a minute.
    pub turns_exhausted: u64,
    /// What the failed calls failed on.
    pub causes: ErrorCauses,
}

/// Why classifier calls failed, split by the distinctions the error type draws.
///
/// A bare `errors=315` was the only trace a tenth of a pass left, and the causes want
/// different responses: a provider that stayed overloaded is worth waiting out and its
/// candidates are worth re-reaching next pass, output the schema rejects is a prompt or
/// schema defect that recurs on every retry, and a missing key means the pass never
/// reached a model at all.
///
/// The buckets are the [`LlmError`] variants, and the reason there are now five is that
/// the error type twice learned a distinction it could not previously draw. First it told
/// a transient refusal from a terminal one: it used to carry the provider's detail as a
/// string, so a 529 overload and a 400 bad request were one bucket, and a run degraded by
/// an outage printed the same line as a run hitting real errors. Then it learned to hear
/// silence — a request the provider never answered at all, which before it was bounded
/// was not an error but an indefinite block.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ErrorCauses {
    /// The call failed on something no retry could change.
    pub api: u64,
    /// The provider kept answering 429 or 5xx until the retry allowance ran out.
    ///
    /// Its own bucket because it is the one cause that says *nothing is wrong here*:
    /// the work is still classifiable and the next pass will reach it. Folded into
    /// `api` it reads as breakage, which is how a provider outage came to look like a
    /// classifier defect.
    pub overloaded: u64,
    /// The provider never answered, or the classification ran out of wall clock.
    ///
    /// Separate from `overloaded` even though both are worth re-reaching, because they
    /// are different observations about the provider and point at different things to
    /// check: a 529 proves a model was reached and is answering, while this says nothing
    /// came back at all — a hung socket, a saturated network, or a session whose agentic
    /// loop is too expensive to finish. A pass reading `timeout=180` is a connectivity
    /// or cost question; one reading `overloaded=180` is a capacity question.
    pub timeout: u64,
    /// The model answered with something that is not a verdict.
    pub parse: u64,
    /// No API key was configured for the call.
    pub missing_api_key: u64,
}

impl ErrorCauses {
    /// Attributes one failure to the cause its variant names.
    const fn record(&mut self, error: &LlmError) {
        let cause = match error {
            LlmError::Api(_) => &mut self.api,
            LlmError::Overloaded(_) => &mut self.overloaded,
            LlmError::Timeout(_) => &mut self.timeout,
            LlmError::Parse(_) => &mut self.parse,
            LlmError::MissingApiKey(_) => &mut self.missing_api_key,
        };
        *cause += 1;
    }

    /// Names the causes that were actually hit, or nothing when none were.
    ///
    /// A cause at zero is noise: it reports the absence of a failure nobody asked
    /// about, and a clean pass should read as clean.
    fn breakdown(&self) -> String {
        let hit: Vec<String> = [
            ("api", self.api),
            ("overloaded", self.overloaded),
            ("timeout", self.timeout),
            ("parse", self.parse),
            ("missing_api_key", self.missing_api_key),
        ]
        .into_iter()
        .filter(|&(_, count)| count > 0)
        .map(|(name, count)| format!("{name}={count}"))
        .collect();
        if hit.is_empty() {
            return String::new();
        }
        format!(" ({})", hit.join(", "))
    }
}

/// Renders one pass's result as the line an operator reads.
fn summary_line(outcome: &AutoClassifyOutcome) -> String {
    format!(
        "Auto-classify: assigned={}, junked={}, proposed={}, superseded={}, refused={}, \
         missing_stream={}, undetermined={}, turns_exhausted={}, skipped_answered={}, \
         skipped={}, errors={}{}",
        outcome.assigned,
        outcome.junked,
        outcome.proposed,
        outcome.superseded,
        outcome.refused,
        outcome.refused_missing_stream,
        outcome.undetermined,
        outcome.turns_exhausted,
        outcome.skipped_answered,
        outcome.skipped,
        outcome.errors,
        outcome.causes.breakdown(),
    )
}

/// Turns a worker thread's panic into the failed call it amounts to.
///
/// [`LlmError::Api`] is the bucket it belongs in: a panic here is a defect in this
/// process rather than anything the provider did, so no retry changes it — which is what
/// that variant means and what separates it from `overloaded` and `timeout`. The payload
/// is read for its message because a count carrying none is the `errors=315` this
/// module's tally exists to replace.
fn panicked(payload: &(dyn Any + Send)) -> LlmError {
    let reason = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_owned());
    LlmError::Api(format!("classifier worker panicked: {reason}"))
}

impl Resolver<'_> {
    /// Junks the subagents whose `parent_session_id` names a session that was never
    /// indexed.
    ///
    /// A subagent whose parent left no trace has nothing to inherit and no context to
    /// classify against. The known population is a bounded 2026-04 ingest defect, so
    /// this drains rather than growing; recurrence is an ingest bug to fix at the
    /// source, not a fallback classification path to build.
    fn junk_orphan_subagents(&mut self) -> Result<()> {
        let orphans = self
            .db
            .orphan_subagent_ids()
            .context("load subagents whose parent was never indexed")?;
        if orphans.is_empty() {
            return Ok(());
        }
        let stream_id = self
            .db
            .junk_stream_id()
            .context("resolve the reserved junk stream")?;
        for session_id in orphans {
            self.outcome.assigned += self
                .db
                .assign_unassigned_events_by_session_id(
                    &session_id,
                    &stream_id,
                    JUNK_ASSIGNMENT_SOURCE,
                )
                .context("junk a subagent whose parent was never indexed")?;
            self.outcome.junked += 1;
        }
        Ok(())
    }

    /// Routes structurally-junk sessions in bulk, before any budget is spent selecting.
    ///
    /// The same rule [`classify_sessions`](Self::classify_sessions) applies one session at
    /// a time, hoisted ahead of selection. Junk costs no model call, but it was only
    /// recognised *after* `unclassified_user_sessions` had already spent a slot on it, so
    /// it consumed the bound that exists to ration calls. Measured on the live database
    /// over one hour: **840 structurally-junk sessions classified against 20 real ones**,
    /// with pass summaries reading `junked=171..177` of 200 session slots. Model-call
    /// concurrency could not help, because only ~29 real sessions per pass ever reached
    /// the model.
    ///
    /// Routing removes them from the candidate set by *answering* them — their events gain
    /// a stream, so the `EXISTS` clause selection reads stops matching. Filtering them out
    /// of that query instead is forbidden: junk excluded from selection is junk that is
    /// never routed, and it would accumulate forever.
    ///
    /// Both counters move, because the per-session path moves both: the sessions land in
    /// `junked` and their events in `assigned`. Folding in only the first would make a
    /// pass summary understate what it did by exactly the work this step took over.
    fn route_structurally_junk_sessions(&mut self) -> Result<()> {
        let routed = self
            .db
            .route_structurally_junk_sessions(JUNK_ROUTING_PER_PASS)
            .context("route structurally junk sessions to the reserved junk stream")?;
        self.outcome.junked += routed.sessions;
        self.outcome.assigned += routed.events;
        Ok(())
    }

    /// Classifies one bounded pass's worth of sessions, a chunk of calls at a time.
    ///
    /// The model calls inside a chunk run at the same time; everything else stays serial.
    /// Junk is routed in selection order before any call is made, verdicts are applied in
    /// the chunk's own order, and every write goes through the one database connection
    /// this resolver holds. A pass is therefore reproducible: the calls race, the
    /// decisions do not.
    ///
    /// **Why that is safe.** The only state a verdict adds to is the roster, and the
    /// roster is not what decides whether a stream is created. Every call in a chunk
    /// answers from the snapshot the chunk began with, so a stream minted by verdict `k`
    /// is missing from the roster verdicts `k+1..N` of that *same* chunk answered
    /// against. That costs nothing, because [`Resolver::stream_named`] treats the roster
    /// as a cache and the `streams` table as the authority: a name it cannot find in the
    /// snapshot goes to `tt_db::find_stream_by_normalized_name`, which reads the table and
    /// returns the earliest-created match, so several verdicts naming one stream collapse
    /// onto one row instead of minting a second. Chunk `N+1` sees that stream in the
    /// roster regardless, because `resolve` pushes every stream it mints onto it.
    ///
    /// What holds all of that up is that `stream_named` runs during *application*, which
    /// is serial. Its find-and-insert is two operations rather than one transaction, so
    /// two concurrent copies of it would both find nothing and both insert — the
    /// duplicate-stream failure that took 55 renames, 5 merges and 9 dissolves to undo.
    /// Nothing here runs it concurrently, and nothing that does may be added without a
    /// transaction around it.
    fn classify_sessions(&mut self) -> Result<()> {
        let candidates = self
            .db
            .unclassified_user_sessions(SESSIONS_PER_PASS)
            .context("load unclassified user sessions")?;
        let mut askable = Vec::with_capacity(candidates.len());
        for (session, machine) in candidates {
            let prompt_count = u32::try_from(session.user_prompts.len())
                .context("session prompt count exceeds supported range")?;
            // Junk is settled here rather than taking a slot in a chunk, because it costs
            // no model call: a session that ran no tool and held at most one exchange
            // cannot carry work, and structure alone says so.
            if is_structurally_junk(session.tool_call_count, session.message_count) {
                let session_id = session.session_id;
                let stream_id = self.junk(&AssignmentTarget::Session {
                    session_id: &session_id,
                    prompt_count,
                })?;
                self.inherit(&session_id, &stream_id)?;
                continue;
            }
            askable.push((
                ClassificationInput {
                    session_id: session.session_id,
                    machine,
                    cwd: Some(session.project_path),
                    starting_prompt: session.starting_prompt,
                    user_prompts: session.user_prompts,
                    window_titles: Vec::new(),
                    // Orders the roster around when this work happened. A pass drains a
                    // backlog months deep, so ordering around `now` hides the era the
                    // session belongs to entirely.
                    started_at: Some(session.start_time),
                },
                prompt_count,
            ));
        }
        for chunk in askable.chunks(CLASSIFY_CONCURRENCY) {
            let verdicts = self.classify_concurrently(chunk);
            for ((input, prompt_count), verdict) in chunk.iter().zip(verdicts) {
                let Some(output) = self.record_call(&input.session_id, verdict) else {
                    continue;
                };
                let target = AssignmentTarget::Session {
                    session_id: &input.session_id,
                    prompt_count: *prompt_count,
                };
                if let Some(stream_id) = self.resolve(output, target, input.started_at)? {
                    self.inherit(&input.session_id, &stream_id)?;
                }
            }
        }
        Ok(())
    }

    /// Runs one chunk's model calls at the same time, answering in the chunk's order.
    ///
    /// `&self` rather than `&mut self` is the load-bearing half. Two things cross into
    /// the threads: the classifier, which its own trait bound declares `Send + Sync`, and
    /// a shared slice of the roster. The database is not among them and cannot be —
    /// `tt_db::Database` is `Send` but not `Sync`, so a closure reaching for it does not
    /// compile. "Every write stays serial" is therefore a property the compiler enforces
    /// rather than one a later edit has to remember.
    ///
    /// A worker that panics costs its own input and nothing else. Every handle is joined,
    /// so a panic arrives here as a value instead of taking the scope down with it, and it
    /// becomes a failed call: counted, recorded, session left unassigned — the same
    /// resting place a provider error leaves it in.
    fn classify_concurrently(
        &self,
        chunk: &[(ClassificationInput, u32)],
    ) -> Vec<Result<ClassificationOutput, LlmError>> {
        let classifier = self.classifier;
        let roster = self.roster.as_slice();
        std::thread::scope(|scope| {
            // Every call is spawned before any of them is joined. Chaining the two into
            // one lazy iterator would spawn a thread and immediately wait on it, which is
            // the serial loop this replaced wearing a thread — and it is what clippy's
            // `needless_collect` suggests here.
            let mut calls = Vec::with_capacity(chunk.len());
            for (input, _) in chunk {
                calls.push(scope.spawn(move || classifier.classify(input, roster)));
            }
            calls
                .into_iter()
                .map(|call| {
                    call.join()
                        .unwrap_or_else(|payload| Err(panicked(&*payload)))
                })
                .collect()
        })
    }

    fn classify_window_runs(&mut self) -> Result<()> {
        let runs = newest_first(build_unassigned_window_runs(self.db)?);
        for run in runs.into_iter().take(WINDOW_RUNS_PER_PASS) {
            // A run whose answer is already queued is asked again only when the
            // classifier has changed since. Both halves of that are load-bearing, and
            // each undoes a failure the other one caused.
            //
            // Skipping unconditionally meant a run nobody reviewed could never be
            // re-answered, and the classifier has improved under every one of them.
            // Asking unconditionally meant a bounded budget spent re-asking questions
            // already on file: 212 pending window-run proposals against 101 calls a
            // pass, while 71,635 focus events across 149 days went unreached because
            // the budget never got past them. Focus events are direct time, so what the
            // waste costs is the headline number.
            //
            // `tt_llm::CLASSIFIER_GENERATION` is what tells the two apart. A pending
            // proposal this generation authored is a verdict already reached, so asking
            // again buys nothing; anything older — including the `NULL` of every row
            // predating the column — is re-asked, exactly once, because `propose`
            // re-stamps the question it declines to duplicate.
            //
            // A run's event set is its identity, so it also leaves the skip by gaining
            // an event, which is why this gate is safe here and is not extended to
            // sessions: a session keeps its id while its prompts grow.
            if self
                .db
                .has_pending_proposal_for_events_at_generation(
                    &run.event_ids,
                    CLASSIFIER_GENERATION,
                )
                .context("check whether this classifier already answered a window run")?
            {
                self.outcome.skipped_answered += 1;
                continue;
            }
            let event_id = run
                .event_ids
                .first()
                .context("window run must contain at least one event")?;
            // A run's own start, so the roster is ordered around the focus being
            // classified. Unreadable rather than absent is left as `None`: the roster then
            // falls back to plain recency, which is degraded but never arbitrary.
            let started_at = DateTime::parse_from_rfc3339(&run.start)
                .map(|start| start.with_timezone(&Utc))
                .inspect_err(|error| {
                    tracing::warn!(
                        start = %run.start,
                        %error,
                        "window run has an unreadable start; ordering its roster by recency"
                    );
                })
                .ok();
            let input = ClassificationInput {
                session_id: format!("window:{event_id}"),
                machine: run.machine_id,
                cwd: None,
                starting_prompt: None,
                user_prompts: Vec::new(),
                window_titles: run.titles,
                started_at,
            };
            if let Some(output) = self.classify(&input) {
                self.resolve(
                    output,
                    AssignmentTarget::Events(&run.event_ids),
                    input.started_at,
                )?;
            }
        }
        Ok(())
    }

    fn recheck_sessions(&mut self) -> Result<()> {
        for (session_id, prompt_count) in self
            .db
            .get_recheck_candidates()
            .context("load automatic classification recheck candidates")?
        {
            let Some((session, machine)) = self
                .db
                .get_agent_session(&session_id)
                .context("load session recheck input")?
            else {
                self.db
                    .mark_rechecked(&session_id)
                    .context("mark missing session recheck complete")?;
                continue;
            };
            let current_prompt_count = u32::try_from(session.user_prompts.len())
                .context("session prompt count exceeds supported range")?;
            if current_prompt_count <= prompt_count {
                continue;
            }
            let input = ClassificationInput {
                session_id: session.session_id,
                machine,
                cwd: Some(session.project_path),
                starting_prompt: session.starting_prompt,
                user_prompts: session.user_prompts,
                window_titles: Vec::new(),
                started_at: Some(session.start_time),
            };
            if let Some(output) = self.classify(&input) {
                let target = AssignmentTarget::Recheck {
                    session_id: &session_id,
                    prompt_count: current_prompt_count,
                };
                if let Some(stream_id) = self.resolve(output, target, input.started_at)? {
                    self.inherit(&session_id, &stream_id)?;
                }
            }
            self.db
                .mark_rechecked(&session_id)
                .context("mark session recheck complete")?;
        }
        Ok(())
    }
}

/// Automatically classifies eligible sessions and unassigned window-focus activity.
///
/// # Errors
/// Returns an error when database reads or writes required to process a candidate fail.
pub fn run_auto(
    db: &tt_db::Database,
    config: &Config,
    classifier: &dyn Classifier,
) -> Result<AutoClassifyOutcome> {
    let mut resolver = Resolver::new(db, config, classifier)?;
    resolver.junk_orphan_subagents()?;
    // Before any phase that selects candidates, because this one *creates* the room those
    // phases need. `unclassified_user_sessions` is bounded so a pass reaches today's work
    // ahead of a backlog, and junk was consuming that bound without ever reaching a model:
    // 840 junk sessions to 20 real ones in one measured hour. Routing settles them here so
    // the bounded selection below is spent on work a model can actually answer.
    resolver.route_structurally_junk_sessions()?;
    // Sessions first. This ordering was inverted once, on the reasoning that focus events
    // are direct time and must not queue behind a session backlog, and the reasoning was
    // sound on its own terms but wrong on the arithmetic that followed it.
    //
    // Measured after the inversion shipped: with the window phase running first at
    // `WINDOW_RUNS_PER_PASS` = 400 and the observed call rate, a pass spent hours in that
    // phase before reaching a session at all. `classified_sessions` sat unmoved at 2,003
    // across 25 minutes while the classifier reported success throughout, the session
    // backlog held at 3,559, and the events waiting on those sessions *grew* by 87.
    //
    // The two phases are not comparable in yield, which is what settles the order. A
    // session classification attributes every event carrying its id — 1,339,657 events
    // across 3,559 sessions, ~376 per call. The window phase places ~11 focus events a
    // day against 400 calls a pass, because a run's payload is a handful of window titles
    // and **0 of 403 pending proposals reach the 0.80 threshold** (best 0.78, window runs
    // average 0.59). Running it first therefore spends the whole pass on the phase that
    // attributes almost nothing, and starves the one that attributes almost everything.
    //
    // The original concern still holds and is now met by the bound rather than the order:
    // `WINDOW_RUNS_PER_PASS` is an independent budget, so focus attribution cannot be
    // starved of calls by a session backlog. What it can lose is a pass cut short at
    // shutdown — and losing ~11 events a day of a phase that is already near-zero yield
    // is the cheaper of the two failures by four orders of magnitude.
    resolver.classify_sessions()?;
    resolver.classify_window_runs()?;
    resolver.recheck_sessions()?;
    println!("{}", summary_line(&resolver.outcome));
    Ok(resolver.outcome)
}
