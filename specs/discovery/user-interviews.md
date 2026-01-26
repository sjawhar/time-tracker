# User Interviews

## Interview Protocol

### Goals
- Understand current workflows and pain points
- Discover unmet needs
- Validate or invalidate assumptions
- Identify edge cases and exceptions

### Questions Framework
1. Current state: How do you track time today?
2. Workflow: Walk me through a typical work session
3. Pain points: What frustrates you about current solutions?
4. Workarounds: What hacks have you developed?
5. Ideal state: If you could wave a magic wand, what would you have?
6. Priorities: What's essential vs nice-to-have?

---

## Interview #1: Sami

**Date**: 2025-01-25
**Role**: Developer using AI coding agents
**Environment**: Remote development via SSH/tmux

### Current Workflow

**Environment:**
- Remote development via SSH/tmux on devpod
- Multiple tmux sessions in a session group (dev, dev2, dev3 sharing same panes)
- 11 panes across 3 windows, each pane typically running Claude Code
- Uses Wispr Flow for voice dictation - clicks away from active pane while dictating

**Current time tracking:**
- Toggl with desktop activity tracking + calendar integration
- Desktop tracking now useless - just sees "iTerm2 - ssh"
- Stopped manual start/stop - too much overhead with interleaved work
- Weekly runs `fill-toggl` script to retroactively fill from Claude session logs
- Result: "Very lossy" - big time blocks hard to attribute, sessions have random IDs

**Work pattern:**
- 10+ agents running concurrently across panes
- Constantly bouncing between them: provide feedback → submit → switch to next
- Context switches every few minutes
- Active pane ≠ current attention (due to dictation workflow)

### Pain Points

1. **No granular visibility** - SSH session is opaque to desktop trackers
2. **Interleaved work breaks linear model** - Can't fill "gaps" when work is parallel
3. **Retroactive attribution is hard** - Session logs aren't labeled with tasks
4. **Manual categorization takes too long** - Spending too much time on timesheet admin
5. **Can't distinguish human vs agent time** - Both valuable but different
6. **No priority alignment view** - Don't know if time matches priorities

### Key Quotes

> "I might have 10 or more [agents] going at once. I'm not manually starting and stopping tasks anymore."

> "You can't necessarily trust that the currently active pane always corresponds to the thing I'm currently working on because I might click away while I'm dictating."

> "I think one metric of healthy development practice is the ratio of agent time to human time. If you're able to get much more agent time out of each unit of human time, that's a well-managed project."

> "We've got to really be creative here about ways that we can do work for the user."

> "We're going to have to go beyond the simple deterministic rules that have governed previous time trackers and do something really smart using LLMs."

### Desired Capabilities

**Core:**
- Automatic stream detection from tmux panes, directories, Claude sessions
- Minimal manual input (<30 min/week on categorization)
- Weekly breakdown by project and activity type
- Historical reporting

**Priority alignment:**
- Tie streams to priorities/todos (Linear issues)
- "Am I spending time on the right things?" view
- Priority-weighted alignment score

**Agent metrics:**
- Human:agent time ratio per task
- Which task types benefit most from parallelization
- Capacity planning insights

**Real-time:**
- Live view of current active stream + background agent streams
- Timeline view of today's streams

### Data & Architecture Requirements

**Storage:**
- Data lives on remote, syncs to laptop as canonical home
- Could use Notion DB, S3, or custom solution
- Shared project/tag taxonomy across multiple remotes

**Interface:**
- TUI dashboard (primary)
- CLI commands for everything (agent-accessible)
- API/MCP tools for programmatic access

**Detection:**
- Automatic stream start: new pane + new directory + new Claude session
- Fallback: tmux keyboard shortcut to explicitly start stream
- LLM-powered activity type inference from session content
- Debouncing for rapid pane switches

**Privacy:**
- Don't retain raw session logs
- Process ephemerally, store only derived metadata

### Activity Types (Tags)

Admin, Code Review, Data, Design, Development, Documentation, Events, Meetings, Messages, Ops, Planning, Reading, Research, Training, Writing

### Success Criteria (1 month)

- Good understanding of where time is spent
- <30 min/week on explicit categorization
- Passive tracking that just works
- Breakdown by project and activity type
- Historical reporting available

### Insights

1. **It's not a time tracker, it's a priority alignment system** - Time is just one signal
2. **Stream, not session** - A "stream" is a coherent unit of work that can span multiple Claude sessions
3. **Parallel, not linear** - The mental model is concurrent streams, not gaps to fill
4. **Human attention is the scarce resource** - Agent time is cheap, human time is valuable
5. **LLM-native classification** - Deterministic rules won't cut it for activity type inference
6. **Distributed-first** - Multiple remotes, sync to canonical home
7. **Minimal friction** - Automatic detection, only ask human when truly needed

---

## Interview Summary

_To be compiled after interviews complete._
