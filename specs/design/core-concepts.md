# Core Concepts

## Mental Model

Traditional time trackers assume **one thing at a time**. You start a timer, work, stop it. If there's a gap, you fill it. This model breaks down when:

- You have 10+ AI agents running in parallel
- You're constantly context-switching between them
- Work happens *concurrently*, not sequentially
- The currently active window doesn't reflect where your attention is

River uses a different model: **observe events, infer streams, distinguish human attention from agent execution**.

### Key Shifts

| Traditional Tracker | River |
|---------------------|-------|
| One task at a time | Multiple concurrent streams |
| Start/stop timers | Events are observed automatically |
| Fill gaps in timeline | Time can overlap (parallel work) |
| Track hours worked | Track attention given + work delegated |
| Retrospective logging | Priority alignment feedback loop |

### The Core Insight

**Human attention is the scarce resource.** Agent time is cheap and parallelizable. A well-managed workflow maximizes the ratio of delegated work to direct attention — not by minimizing your involvement, but by making every moment of your attention count.

---

## Key Abstractions

River has four core concepts:

### 1. Event

An **event** is an atomic observation — a raw fact with a timestamp.

Examples:
- "tmux pane 3 focused at 14:32:07"
- "Claude session started in /home/user/project-x at 14:32:15"
- "git commit in /home/user/project-x at 15:47:22"

Events are **immutable**. Once recorded, they don't change. Everything else in River is derived from events.

Events have:
- `timestamp` — when it happened
- `type` — what kind of event (pane_focus, session_start, git_commit, etc.)
- `source` — where it came from (tmux, claude, git, manual)
- `context` — additional data (directory, session_id, pane_id, etc.)

### 2. Stream

A **stream** is a coherent unit of work — a grouping of related events that represent meaningful activity.

Streams are **inferred**, not manually started/stopped. The system observes events and clusters them into streams based on:
- Directory/repository
- Temporal proximity
- Session relationships
- LLM-powered semantic analysis

A stream might span:
- Multiple Claude sessions
- Multiple tmux panes
- Multiple days
- Gaps where you stepped away

Streams are **derived from events**. This means:
- Modifying or creating events can automatically create or change streams
- Users can manually create streams by adding events to them
- Stream boundaries are interpretations, not hard facts

Streams are the primary unit for reporting. "How much time did I spend on project X?" is answered by summing the streams associated with that project.

Streams have:
- `id` — unique identifier
- `events` — the events that comprise this stream
- `tags` — flexible classification (see below)
- `time_direct` — total human attention time
- `time_delegated` — total agent execution time

### 3. Direct / Delegated Time

River distinguishes two types of time within a stream:

**Direct time** — moments when you are actively engaged:
- Reading agent output
- Providing input or feedback
- Making decisions
- Reviewing code
- Thinking about the problem

**Delegated time** — moments when agents work autonomously:
- AI generating code while you work on something else
- Tests running in the background
- Builds compiling
- Any agent execution without your active attention

Both types of time are valuable, but they represent different things:
- Direct time is your **attention** — the scarce resource
- Delegated time is **leverage** — work happening because of your direction

A key metric: the **delegation ratio** (delegated ÷ direct). Higher ratios suggest effective parallelization — each unit of your attention produces more output.

#### Attention Allocation (Overlapping Streams)

When multiple streams are active simultaneously (common with parallel agents), direct time must be **allocated** across them. Time needs to add up to something sensible for billing and reporting.

The allocation model:
- At any moment, your direct attention can only be on **one stream**
- Delegated time can accumulate on **multiple streams** in parallel
- When streams overlap, River allocates direct time based on which stream has focus
- Total direct time across all streams ≤ wall clock time (no double-counting attention)
- Total delegated time can exceed wall clock time (parallel execution)

Example: From 2pm-3pm, you have three agents running. You spend 20 minutes directing agent A, 15 minutes on agent B, and 10 minutes on agent C. The remaining 15 minutes you're away.
- Stream A: 20 min direct, 60 min delegated
- Stream B: 15 min direct, 60 min delegated
- Stream C: 10 min direct, 60 min delegated
- Total direct: 45 min (sums correctly)
- Total delegated: 180 min (3x parallelization)

### 4. Tag

A **tag** is flexible metadata attached to a stream. Tags enable categorization, filtering, and reporting without imposing a rigid structure.

Examples:
- Activity types: `development`, `code-review`, `research`, `documentation`
- Projects: `project:river`, `project:client-website`
- Clients: `client:acme-corp`
- Priority: `high-priority`, `tech-debt`
- Custom: anything useful for your workflow

Tags are:
- **Multiple per stream** — a stream can have many tags
- **Flat** (for MVP) — no hierarchy, just strings
- **User-defined** — no enforced taxonomy
- **LLM-inferred** — the system suggests tags based on content, you confirm or override

#### Future: Typed Tags

Post-MVP, tags may evolve to carry properties (like Tana's supertags):
- `project:river` could have fields: `status: active`, `repo: github.com/...`
- `client:acme` could have fields: `billable: true`, `rate: $150/hr`

For MVP, tags are simple strings. The system doesn't distinguish "project tags" from "activity tags" — that's a convention you choose.

---

## User Operations

While River infers streams automatically, users have full control to correct and refine:

### Event Operations
- **Create event** — manually log something that wasn't captured (e.g., "I was thinking about this from 2-3pm")
- **Modify event** — change timestamp, type, or context of an event
- **Delete event** — remove an erroneous event
- **Move event** — reassign an event from one stream to another

### Stream Operations
- **Create stream** — explicitly start a new stream (adds a manual event that seeds it)
- **Merge streams** — combine two streams into one (all events consolidated)
- **Split stream** — divide a stream at a point in time
- **Retag stream** — add, remove, or change tags

### Correction Philosophy

The goal is **observe first, correct rarely**. If users spend significant time fixing inferences, the system has failed. But when correction is needed, it should be:
- Fast (single command or click)
- Non-destructive (events preserved, interpretations changed)
- Propagating (fixing one thing updates derived views)

Tags can help track correction needs. A user might tag streams as `needs-review` or `wasted-work` to flag areas for attention.

---

## Glossary

| Term | Definition |
|------|------------|
| **Event** | An atomic, immutable observation with a timestamp. The raw input to River. |
| **Stream** | A coherent unit of work, inferred from related events. The primary unit for reporting. |
| **Direct time** | Time when the human is actively engaged — reading, deciding, providing input. |
| **Delegated time** | Time when agents work autonomously without human attention. |
| **Delegation ratio** | Delegated time ÷ Direct time. Measures leverage from parallelization. |
| **Attention allocation** | How direct time is distributed across overlapping streams. Ensures time sums sensibly. |
| **Tag** | Flexible metadata for categorizing streams. Multiple tags per stream. |
| **Source** | Where an event originates: tmux, claude, git, manual entry, etc. |

---

## Non-Concepts

Things River intentionally does **not** have (at least for MVP):

| Avoided Concept | Why |
|-----------------|-----|
| **Timer** | No start/stop. Events are observed; time is derived. |
| **Task** | Too granular. Streams are the natural unit; tasks live in external systems (Linear, etc.). |
| **Project (as entity)** | For MVP, projects are just a tagging convention (`project:xyz`). May become first-class later when Linear integration matters. |
| **Activity Type (as entity)** | Just tags. No enforced taxonomy — use what works for you. |
| **Gaps** | Time isn't a line with holes. Parallel streams can overlap. |

---

## Design Principles

1. **Observe, don't interrogate.** Capture events passively. Only ask the human when you truly can't infer.

2. **Streams, not timers.** Work is continuous and overlapping. Don't force artificial start/stop boundaries.

3. **Attention is scarce; delegation is leverage.** Track both, but know which is the bottleneck.

4. **Tags over taxonomies.** Let users build their own structure. Don't impose categories.

5. **Events are immutable; interpretations evolve.** Raw events never change. How they're clustered into streams can be refined.

6. **LLM-native classification.** Deterministic rules can't infer activity type from session content. Use LLMs where they add value.

---

## Deferred to Technical Design

These areas are critical but require deeper technical specification:

- **Stream inference algorithm** — How exactly events are clustered into streams. What signals, what thresholds, what heuristics.
- **Direct/Delegated detection** — How to determine when human attention is present vs. agent-only execution.
- **LLM classification pipeline** — What inputs (metadata only? full transcripts?), what prompts, what cost/latency tradeoffs.
- **Confidence scoring** — How certain is an inference? When to flag for user review.

See `architecture/` for technical specifications.
