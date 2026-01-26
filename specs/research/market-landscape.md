# Market Landscape

## Existing Time Tracking Solutions

### Traditional Time Trackers

#### Toggl Track
- Manual start/stop with timers
- Desktop app with automatic window tracking
- API for programmatic entry creation
- Weakness: Window-based tracking blind to SSH/tmux internals

#### Clockify
- Similar to Toggl, free tier available
- Automatic tracking based on window focus
- API available
- Same blindness to remote development

#### Timely
- AI-powered "memory" that reconstructs your day
- Background activity capture
- "Done-for-you timesheets"
- Cloud-based, privacy concerns for some users
- Still fundamentally window-focused

### CLI Time Trackers

#### Watson
- CLI time tracker with start/stop/report
- Project and tag support
- Local storage (JSON)
- Manual operation - no automatic tracking
- GitHub: https://github.com/jazzband/Watson

#### Timewarrior
- From the Taskwarrior team
- CLI-based, integrates with Taskwarrior
- Manual start/stop
- No automatic tracking

#### timetrap
- Ruby-based CLI tracker
- "Auto sheets" feature for per-directory tracking
- Still manual start/stop

#### ti
- Minimal Python CLI tracker
- JSON storage
- Very simple, manual operation

#### klog
- Plain-text time tracking
- Human-readable file format
- Manual entry

### Developer-Focused Trackers

#### WakaTime
- Automatic tracking via editor plugins
- Terminal plugins for bash/zsh/fish
- Dashboard with language/project breakdowns
- **Note**: WakaTime themselves recommend *against* terminal plugins, preferring editor plugins
- Terminal tracking puts all time in generic "Terminal" project or infers from CWD
- https://wakatime.com/terminal-time-tracking

### AI/Agent-Specific Tools

#### AgentBase
- Multi-agent orchestrator for Claude Code, Cursor, Codex
- Tracks agent sessions and progress
- Focus on coordination, not time tracking
- GitHub: https://github.com/AgentOrchestrator/AgentBase

#### claude_telemetry
- OpenTelemetry wrapper for Claude Code
- Logs tool calls, token usage, costs
- Focus on observability, not time attribution
- GitHub: https://github.com/TechNickAI/claude_telemetry

#### claude-code-log
- Converts Claude Code JSONL transcripts to HTML
- Useful for review, not real-time tracking
- GitHub: https://github.com/daaain/claude-code-log

---

## Gap Analysis

| Need | Traditional Trackers | CLI Trackers | WakaTime | Agent Tools |
|------|---------------------|--------------|----------|-------------|
| Automatic tracking | Partial (window) | No | Yes (editor) | Partial |
| Works in tmux/SSH | No | Yes (manual) | Partial | Yes |
| Interleaved tasks | No | No | No | No |
| Agent time tracking | No | No | No | Partial |
| Fractional attribution | No | No | No | No |
| Human vs agent time | No | No | No | No |
| Event-sourced | No | No | No | No |

**Key Gap**: No solution handles interleaved, multi-context work with automatic attribution and human/agent time distinction.

---

## Opportunity

Build a time tracker that:
1. Is native to tmux-based remote development
2. Automatically attributes time across interleaved contexts
3. Distinguishes human attention from agent computation
4. Uses event sourcing for flexibility and auditability
5. Exports to existing tools (Toggl, Clockify) for billing workflows
