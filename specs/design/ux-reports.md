# UX: Reports

_To be designed after user stories are established._

## Report Types

_What reports do users need?_

## Formats

_What output formats are required?_

## Examples

_Sample report outputs._

---

## Preliminary Ideas

> **Note**: The following are preliminary ideas from early brainstorming. They should be validated against user stories and refined before implementation.

### Daily Report Example

```
═══════════════════════════════════════════════════════════════
  TIME REPORT: January 24, 2024
═══════════════════════════════════════════════════════════════

  SUMMARY
  ───────
  Total Tracked:    8h 15m
  Human Time:       5h 42m (69%)
  Agent Time:       2h 33m (31%)
    └─ Supervised:  1h 15m
    └─ Autonomous:  1h 18m

  BY PROJECT
  ──────────
  acme-webapp                               4h 30m  ████████░░
    Fix auth bug (#1234)                    2h 15m
    Dashboard feature (#1235)               1h 45m
    Code review                             30m

  personal                                  2h 45m  █████░░░░░
    claude-code contributions               1h 30m
    Learning Rust                           1h 15m

  admin                                     1h 00m  ██░░░░░░░░
    Email, meetings                         1h 00m

  AGENT EFFICIENCY
  ────────────────
  Tokens used: 847,231
  Estimated cost: $12.43
  Agent-assisted contexts: 4
  Avg agent session: 38m
```

### MVP Report Scope

For MVP, only need:
- `tt report --week` showing time breakdown by project/tag
- Direct vs delegated time split
- Simple text output (no fancy formatting required)

### Export Formats (Post-MVP)

Toggl CSV format for import:
```csv
Email,Start date,Start time,End date,End time,Duration,Project,Client,Description,Tags
user@example.com,2024-01-15,09:00:00,2024-01-15,11:30:00,02:30:00,acme-webapp,Acme Corp,Fix auth bug,bug;urgent
```
