# UX: Command-Line Interface

_To be designed after user stories are established._

## Design Principles

_What principles guide CLI design?_

## Command Structure

_What commands are available?_

## Examples

_Usage examples for common workflows._

---

## Preliminary Ideas

> **Note**: The following are preliminary ideas from early brainstorming. They should be validated against user stories and refined before implementation.

### Command Sketch

```bash
# Status
tt status                    # Show current contexts, running agents, today's summary
tt status --watch            # Live updating status

# Quick operations
tt start "task description"  # Start explicit tracking (creates context)
tt stop                      # Stop explicit tracking
tt note "made progress on X" # Add annotation to current time

# Queries
tt today                     # Today's time breakdown
tt week                      # This week's summary
tt report --from 2024-01-01 --to 2024-01-31 --project acme
tt report --client "Acme Corp" --format csv

# Context management
tt contexts                  # List active contexts
tt context show <id>         # Show context details
tt context close <id>        # Archive a context

# Agent time analysis
tt agents                    # Show active agent sessions
tt agent-time --week         # Agent vs human time breakdown
tt agent-cost --month        # Token usage and costs by context

# Configuration
tt config                    # Show current config
tt rules                     # Show/edit context rules
tt calibrate                 # Interactive calibration wizard
```

### Prototype CLI (Minimal)

For the data collection prototype, only these commands are needed:

```bash
tt ingest                    # Receive events (called by tmux hooks)
tt events                    # Dump raw events (for debugging)
```
