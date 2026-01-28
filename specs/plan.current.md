# Current Task: Set up Python project with `uv`

## Task from plan.md
> Set up Python project with `uv`

## Spec Reference
- `specs/implementation/tech-stack.md` — specifies Python 3.11+, `uv`, `click` or `typer`, `pydantic`, `anthropic`, `pytest`

## Acceptance Criteria
1. Python project structure with `pyproject.toml`
2. Dependencies installed via `uv`
3. CLI entry point configured (`tt` command)
4. Tests runnable via `pytest`
5. Project follows existing codebase patterns

## Implementation Approach

### Directory Structure
```
tt-local/
├── pyproject.toml
├── tt_local/
│   ├── __init__.py
│   └── cli.py
└── tests/
    └── test_cli.py
```

**Rationale**: Flat layout (no `src/`) is simpler for an application CLI. `uv run` handles imports correctly. The package is named `tt-local` to distinguish from the Rust remote CLI.

### Naming Strategy

The Rust CLI (`crates/tt-cli`) runs on **remote servers** (fast startup for tmux hooks). The Python CLI runs on the **local laptop** only. Since these run on different machines, both can use `tt` as the command name without conflict.

### Dependencies

Core:
- `click>=8.0` — CLI framework (simpler than typer, widely used)
- `pydantic>=2.0` — Event schema validation

Dev:
- `pytest>=8.0` — Testing

Deferred (add in subsequent tasks):
- `anthropic` — Only needed for LLM features (MVP)

### pyproject.toml Configuration

```toml
[project]
name = "tt-local"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = [
    "click>=8.0",
    "pydantic>=2.0",
]

[project.scripts]
tt = "tt_local.cli:main"

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[dependency-groups]
dev = ["pytest>=8.0"]
```

### CLI Stub

```python
# tt_local/cli.py
import click

@click.group()
def main():
    """Time Tracker local CLI."""
    pass

if __name__ == "__main__":
    main()
```

This provides the minimal entry point. Subsequent tasks will add subcommands (`import`, `sync`, `events`, `status`).

## Files to Create

| File | Purpose |
|------|---------|
| `tt-local/pyproject.toml` | Project metadata and dependencies |
| `tt-local/tt_local/__init__.py` | Package marker |
| `tt-local/tt_local/cli.py` | CLI entry point |
| `tt-local/tests/test_cli.py` | Basic test |

## Test Cases

1. `tt --help` outputs help text without error
2. `tt` with no args shows available commands
3. Package imports without error (`from tt_local import cli`)

## Open Questions

None — spec is complete and approach is straightforward.

## Verification Steps

1. Run `uv sync` in `tt-local/` — dependencies install
2. Run `uv run tt --help` — CLI works
3. Run `uv run pytest` — tests pass
