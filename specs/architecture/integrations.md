# Integrations

_To be designed based on user requirements._

## Input Integrations

_Where does data come from?_

- tmux
- Agent session logs
- Git hooks
- Manual input

## Output Integrations

_Where does data go?_

- Export formats
- External APIs
- Webhooks

---

## Preliminary Ideas

> **Note**: The following are preliminary ideas from early brainstorming. They should be validated during design.

### Context Rules Engine

Users could define rules for automatic context assignment:

```yaml
# ~/.config/tt/rules.yaml
rules:
  - match:
      path: "~/work/acme/*"
    assign:
      project: "acme"
      client: "Acme Corp"

  - match:
      git_branch: "feature/*"
    assign:
      tags: ["feature-work"]

  - match:
      tmux_session: "personal-*"
    assign:
      billable: false

  - match:
      agent: "claude-code"
      path: "*/claude-code/*"
    assign:
      project: "claude-code-contributions"
```

**Note**: For MVP, this may be unnecessary if LLM-suggested tags work well enough. Consider deferring rules engine.

### Privacy Configuration

```yaml
# ~/.config/tt/config.yaml
privacy:
  exclude_paths:
    - "~/.ssh/*"
    - "~/.gnupg/*"
    - "**/secrets/*"
    - "**/.env"
  exclude_patterns:
    - "*password*"
    - "*secret*"
```

**Note**: Privacy configuration is important but may not be needed for prototype if we only store metadata (not content).

### Export APIs

**Toggl Track API**:
- POST `/api/v9/workspaces/{workspace_id}/time_entries`
- Fields: description, start, stop, duration, project_id, tags

**Clockify API**:
- POST `/api/v1/workspaces/{workspaceId}/time-entries`
- Similar fields

Both support CSV import as fallback.

### Webhooks (Post-MVP)

```yaml
# ~/.config/tt/config.yaml
webhooks:
  - url: https://example.com/time-webhook
    events: ["context_switch", "report_generated"]
    secret: "webhook-secret"
```

### API Server (Post-MVP)

```bash
# Start server
tt serve --port 8080

# Endpoints
GET  /api/v1/status           # Current tracking status
GET  /api/v1/contexts         # List contexts
POST /api/v1/events           # Submit events (for custom integrations)
GET  /api/v1/report           # Generate report
GET  /api/v1/time/:date       # Time entries for date
```
