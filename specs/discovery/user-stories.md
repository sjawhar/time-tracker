# User Stories

_To be derived from user discovery. Stories should trace back to specific pain points and interview insights._

## Format

```
As a [persona],
I want to [action],
So that [outcome].

Acceptance Criteria:
- [ ] Criterion 1
- [ ] Criterion 2

Source: Pain point P#, Interview #
Priority: Must-have / Should-have / Nice-to-have
```

---

## Stories

### Epic: Automatic Stream Detection

#### US-001: Detect streams from tmux activity

As a parallel agent orchestrator,
I want work streams to be automatically detected when I create new tmux panes and start Claude sessions,
So that I don't have to manually start/stop tracking.

Acceptance Criteria:
- [ ] New pane + new directory + new Claude session creates a new stream
- [ ] Stream inherits semantic description from pane title or session content
- [ ] Rapid pane switches are debounced (not each one a new stream)
- [ ] Can manually trigger new stream via tmux keybinding as fallback

Source: Pain points P1, P4, P5; Interview #1
Priority: Must-have (MVP)

#### US-002: Cluster streams by project and activity type

As a parallel agent orchestrator,
I want streams to be automatically attributed to projects and activity types,
So that I don't have to manually categorize each one.

Acceptance Criteria:
- [ ] LLM-powered inference from session content, pane title, directory
- [ ] Matches against user-defined project and tag taxonomy
- [ ] Suggests new project when work doesn't fit existing ones
- [ ] Only prompts for human input when confidence is low

Source: Pain points P2, P6, P7; Interview #1
Priority: Must-have (MVP)

---

### Epic: Reporting

#### US-003: Weekly breakdown report

As a parallel agent orchestrator,
I want a weekly breakdown of time by project and activity type,
So that I can see where my time actually went.

Acceptance Criteria:
- [ ] Configurable week start day
- [ ] Breakdown by project (toggleable)
- [ ] Breakdown by activity type/tag (toggleable)
- [ ] Historical data accessible (previous weeks)

Source: Pain point P8; Interview #1
Priority: Must-have (MVP)

#### US-004: Daily timeline view

As a parallel agent orchestrator,
I want to see a timeline of today's streams,
So that I can review and adjust categorization if needed.

Acceptance Criteria:
- [ ] Shows all streams for selected day
- [ ] Can toggle between days
- [ ] Can edit stream metadata (project, tag, description)
- [ ] Visual representation of parallel/overlapping streams

Source: Interview #1
Priority: Must-have (MVP)

---

### Epic: Human vs Agent Time

#### US-005: Track human attention separately from agent computation

As a parallel agent orchestrator,
I want to distinguish time I spent actively engaged from time agents worked in the background,
So that I can measure my efficiency and plan capacity.

Acceptance Criteria:
- [ ] Human attention time = periods of active pane focus + input
- [ ] Agent background time = agent activity without human attention
- [ ] Both attributed to same stream/project
- [ ] Can report on human:agent ratio per project/task type

Source: Pain point P3; Interview #1
Priority: Should-have (post-MVP)

#### US-006: Identify highly parallelizable work patterns

As a parallel agent orchestrator,
I want to see which types of tasks benefited most from agent parallelization,
So that I can plan similar tasks to take less of my active time.

Acceptance Criteria:
- [ ] Metrics on agent:human time ratio by task type
- [ ] Identify tasks where parallelism was high
- [ ] Surface patterns (e.g., "infrastructure tasks parallelize 5:1")

Source: Pain point P10; Interview #1
Priority: Should-have (post-MVP)

---

### Epic: Priority Alignment

#### US-007: Priority-weighted alignment score

As a parallel agent orchestrator,
I want to see a score showing how well my time aligned with my stated priorities,
So that I can ensure I'm working on the right things.

Acceptance Criteria:
- [ ] Integrates with priority source (Linear, or internal)
- [ ] Weights time by priority level of associated tasks
- [ ] Shows "alignment score" in weekly report
- [ ] Highlights misalignment (low-priority work getting lots of time)

Source: Pain point P8, P9; Interview #1
Priority: Should-have (post-MVP)

---

### Epic: Real-Time Dashboard

#### US-008: Live view of current streams

As a parallel agent orchestrator,
I want to see what I'm currently working on plus all background agent streams,
So that I have visibility into my parallel work in real-time.

Acceptance Criteria:
- [ ] Shows currently focused stream (human attention)
- [ ] Shows all active background streams
- [ ] Updates in real-time as pane focus changes
- [ ] Shows stream metadata (project, time elapsed)

Source: Interview #1
Priority: Should-have (post-MVP)

---

### Epic: Manual Entry & Editing

#### US-009: Manually add time entries

As a parallel agent orchestrator,
I want to manually add time entries for work that wasn't automatically captured,
So that my records are complete.

Acceptance Criteria:
- [ ] Can create entry with start time, end time, project, tag, description
- [ ] Entries integrate with automatic streams in reports
- [ ] Can edit existing stream metadata

Source: Interview #1
Priority: Should-have (post-MVP)

---

### Epic: Multi-Environment Sync

#### US-010: Sync data across multiple remote environments

As a parallel agent orchestrator,
I want data from multiple remote dev environments to sync to a canonical home,
So that I have a unified view of my work.

Acceptance Criteria:
- [ ] Daemon runs on each remote, captures local data
- [ ] Data syncs to canonical home (laptop or designated server)
- [ ] Shared project/tag taxonomy across all remotes
- [ ] No data loss if network is intermittent

Source: Interview #1
Priority: Nice-to-have (later)

---

## Story Map

```
                         MVP                    Post-MVP              Future
                    ─────────────────     ─────────────────     ─────────────────
Detection           US-001 (streams)
                    US-002 (clustering)

Reporting           US-003 (weekly)       US-007 (priority)
                    US-004 (timeline)

Human/Agent                               US-005 (distinguish)
                                          US-006 (patterns)

Real-time                                 US-008 (live view)

Manual                                    US-009 (add/edit)

Multi-env                                                       US-010 (sync)
```

---

## Priority Summary

**MVP (Must-have):**
- US-001: Auto stream detection
- US-002: Auto clustering/attribution
- US-003: Weekly breakdown report
- US-004: Daily timeline view

**Post-MVP (Should-have):**
- US-005: Human vs agent time
- US-006: Parallelization patterns
- US-007: Priority alignment score
- US-008: Real-time dashboard
- US-009: Manual entry/editing

**Future (Nice-to-have):**
- US-010: Multi-environment sync
