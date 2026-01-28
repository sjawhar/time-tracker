# ADR-002: Remote Analysis Architecture

## Status

Proposed

## Context

The time tracker captures events on remote servers (tmux pane focus, Claude Code sessions) and syncs them to the local machine for analysis. For the attention allocation algorithm to work correctly, we need to map events to streams (coherent units of work, typically corresponding to projects).

### Current State

Events currently include a `cwd` field:
- `tmux_pane_focus` events include the pane's current working directory
- Claude Code events include `cwd` from the session
- Stream inference (not yet implemented) will group events by directory + temporal clustering

### The Problem

The `cwd` field alone is insufficient for accurate stream mapping:

| Problem | Example |
|---------|---------|
| **Subdirectory ambiguity** | `/home/sami/project-x/frontend` and `/home/sami/project-x/backend` are the same project |
| **Utility directories** | `/tmp` or `/var/log` have no project association |
| **Shared directories** | Home directory `~` could span multiple projects |
| **Monorepo complexity** | Different packages in a monorepo may share a root but represent different work |

### Prior Art

**WakaTime** (editor plugin):
- Uses `.wakatime-project` marker file for explicit project mapping
- Falls back to VCS root detection (git/hg/svn)
- Reference: [WakaTime Plugin Guide](https://wakatime.com/help/creating-plugin)

**ActivityWatch aw-watcher-tmux**:
- Captures `session_name`, `window_name`, `pane_title`, `pane_current_command`, `pane_current_path`
- Relies solely on `pane_current_path` for project detection
- No explicit project→directory mapping

**Key Insight**: Most tools rely on VCS root detection as the primary signal. This is automatic (no configuration), reliable (VCS directories are authoritative), and consistent (same project always resolves to same root).

## Decision

Enhance remote `tt ingest` to detect the project root from `cwd` and include both in the event payload.

### Detection Algorithm

Walk up from `cwd` toward filesystem root. At each directory level, check markers in priority order:

```
for dir in [cwd, cwd.parent(), cwd.parent().parent(), ...]:
    if dir contains .tt-project or .wakatime-project:
        return (dir, read_project_name(file))
    if dir contains .git or .jj or .hg or .svn:
        return (dir, dir.name())
    if dir contains package.json or Cargo.toml or pyproject.toml or go.mod:
        return (dir, dir.name())
    if depth_count >= 20:
        break
return null  # No project detected
```

**Priority order at each level**: marker file > VCS directory > package file

**First match wins**: The innermost directory with any marker becomes the project root. This means a `.tt-project` file in a subdirectory overrides the VCS root in a parent directory.

**Implication for monorepos**: A monorepo with `.git` at root and `package.json` in subdirectories will detect the subdirectory's `package.json` as the project root (innermost match). To group all subdirectories under the VCS root, use `.tt-project` at the monorepo root.

### Event Payload Enhancement

Current `tmux_pane_focus` event:
```json
{
  "type": "tmux_pane_focus",
  "cwd": "/home/sami/project-x/frontend/src"
}
```

Enhanced event:
```json
{
  "type": "tmux_pane_focus",
  "cwd": "/home/sami/project-x/frontend/src",
  "project_root": "/home/sami/project-x",
  "project_name": "project-x"
}
```

Fields:
- `project_root`: Absolute path to detected project root (null if none detected)
- `project_name`: Directory name of project root (null if none detected)

### Where Detection Runs

Detection runs at **ingest time on remote**.

| Option | Decision |
|--------|----------|
| Remote (at ingest time) | **Selected** — Fresh detection, correct filesystem context |
| Local (at import time) | Rejected — Local doesn't have the remote filesystem |
| Remote (at export time) | Rejected — `cwd` may have changed since event was captured |

### Detection Parameters

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| **Cache size** | 100 entries (LRU) | Covers typical workspace size |
| **Cache TTL** | 5 minutes | Balance freshness vs performance |
| **Cache scope** | Process lifetime | Simple implementation |
| **Cache key** | Canonicalized `cwd` path | Handles symlinks consistently |
| **Negative caching** | 1 minute TTL | Shorter TTL so new projects are detected quickly |
| **Timeout** | 100ms best-effort | See Timeout Semantics below |
| **Depth limit** | 20 parent() calls from cwd | Prevents runaway traversal |

**Timeout Semantics**: The 100ms timeout is best-effort. Filesystem syscalls (stat, readdir) are typically not interruptible mid-call. A single slow stat() on a network mount may exceed 100ms. The timeout is checked between filesystem operations, not during them. This is documented as a known limitation.

### `.tt-project` File Specification

The `.tt-project` file creates a project boundary at its location.

**Format**: Plain text. First line is the project name, trimmed of leading/trailing whitespace. Subsequent lines are ignored (reserved for future use).

```
my-custom-project-name
```

**Validation**:
- Maximum 256 characters for project name
- Reject control characters (0x00-0x1F except newline)
- If first line is empty/whitespace-only, treat as if file doesn't exist (continue walking up)
- Project name is display-only; stream grouping uses `project_root` path

**Precedence**: If both `.tt-project` and `.wakatime-project` exist in the same directory, prefer `.tt-project`. Format for `.wakatime-project` is identical.

**Use cases**:
- Override automatic detection when VCS root grouping isn't desired
- Split a monorepo into separate tracked projects
- Mark directories without VCS (docs folders, design assets) as belonging to a project

### Edge Cases

| Case | Behavior |
|------|----------|
| **No project detected** | `project_root` = null; stream inference uses `cwd` |
| **Nested VCS roots** | Use innermost root (first found walking up) |
| **Symlinks in cwd** | Resolve to canonical path once at start, then walk without following further symlinks |
| **Network mounts** | Detection may exceed 100ms (best-effort timeout); falls back to `cwd` with warning log |
| **Permission denied during walk** | Stop and use original `cwd` as fallback; log at WARN level |
| **Directory not found during walk** | Same as permission denied; use `cwd` fallback |
| **`.git`/`.jj` is a file (worktrees)** | Read gitdir path but only follow if it's within the same filesystem; otherwise treat as regular VCS marker |
| **Monorepo with subpackages** | Innermost marker wins (package.json before VCS root); use `.tt-project` at root to group all |
| **Empty `.tt-project` file** | Continue walking up as if file doesn't exist |
| **Very long paths** | Depth limit (20 levels) prevents excessive traversal; return null |

### Symlink and Path Safety

**Symlink handling**:
1. Canonicalize input `cwd` once at the start
2. During walk, use `O_NOFOLLOW` equivalent when checking for markers
3. This prevents symlink-based path traversal attacks

**Gitdir/worktree safety**:
1. When `.git` is a file, parse the `gitdir:` line
2. Only follow if the target path is on the same filesystem (same device ID)
3. If target is on different filesystem, treat `.git` file as a regular VCS marker (use its location as project root)
4. This prevents malicious `.git` files from causing cross-filesystem traversal

### Visibility and Diagnostics

Users need to understand what was detected:

| Feature | Location | Description |
|---------|----------|-------------|
| Detection method | `tt status` output | Show "(via .git)" or "(via .tt-project)" next to project name |
| Project root | `tt streams --verbose` | Show `project_root` and detection method per stream |
| Grouping hint | `tt streams` | When multiple paths are grouped under one project, show hint about `.tt-project` |
| **Diagnostic command** | `tt project <path>` | Verify detection without generating events (MVP) |

**`tt project` command** (MVP):

```bash
$ tt project /home/sami/project-x/frontend/src
Project: project-x
Root: /home/sami/project-x
Detected via: .git directory

$ tt project /tmp
No project detected for this path.
```

This allows users to verify their `.tt-project` setup works without the 10+ step debugging loop of generating events → syncing → checking reports.

### Correction Mechanism

For MVP, correction options:

1. **`tt tag`**: Add tags to streams for billing/reporting classification. Does not change underlying `project_root`.
2. **`.tt-project` file**: Create/modify to change future event detection. Does not affect historical events.

Post-MVP consideration: `tt stream reassign` for retroactive reclassification of historical events.

## Consequences

### Positive

- **Accurate grouping**: Subdirectories grouped correctly without manual configuration
- **Automatic**: Works out of the box for VCS-managed projects
- **Backward compatible**: Missing fields fall back gracefully; old events still work
- **Flexible**: `.tt-project` provides escape hatch for edge cases
- **Standards-based**: Compatible with WakaTime ecosystem

### Negative

- **Small overhead**: ~5-10ms added to `tt ingest` (acceptable given 500ms debounce)
- **Network mount risk**: Detection may be slow; timeout is best-effort (see Timeout Semantics)
- **Historical events**: Events captured before this feature won't benefit from improved detection
- **Monorepo grouping**: Innermost marker wins by default; may surprise users expecting VCS root grouping. Mitigated by documentation, `tt streams` grouping hint, and `.tt-project` escape hatch.
- **Event immutability**: Project detection is captured at ingest time. Adding `.tt-project` later doesn't retroactively fix old events.

## Implementation Notes

### Remote (Rust `tt ingest`)

1. Add `detect_project_root(cwd: &Path) -> Option<(PathBuf, String)>` function
2. Canonicalize `cwd` once at start; use as cache key
3. Walk up directory tree checking for markers in priority order at each level
4. Cache results in process-scoped LRU: 5-minute TTL for positive results, 1-minute TTL for negative
5. Add `project_root` and `project_name` fields to event JSON output
6. Handle worktree files (`.git` file pointing to actual git dir) with same-filesystem constraint
7. Respect 100ms best-effort timeout and 20-level depth limit
8. Validate `.tt-project` content: 256 char limit, no control characters
9. Log at DEBUG level: cwd, detected root, method, duration

### Remote (`tt project` diagnostic command)

1. Add `tt project <path>` command that runs detection and outputs result
2. No events generated; purely diagnostic
3. Shows: project name, root path, detection method (or "No project detected")

### Local (Python)

1. Update event parsing to accept optional `project_root`, `project_name`
2. Stream inference algorithm uses `project_root` when present
3. Fall back to `cwd` when `project_root` is missing
4. No configuration changes needed

### Testing

| Test Case | Expected Behavior |
|-----------|------------------|
| `cwd` inside git repo | Detect git root as project |
| `cwd` with `.tt-project` in parent | Use `.tt-project` location as project |
| `cwd` in `/tmp` | No project detected; fields are null |
| Nested git repos | Innermost repo wins |
| Git worktree | Follow `.git` file if same filesystem; else use `.git` location |
| Very deep directory (50+ levels) | Depth limit triggers; return null |
| `.tt-project` in subdir, `.git` in parent | `.tt-project` wins (innermost marker) |
| `package.json` in subdir, `.git` in parent | `package.json` wins (innermost marker) |
| `.tt-project` at monorepo root | All subdirectories grouped under `.tt-project` |
| Empty `.tt-project` | Continue walking up; may find VCS root |
| `.tt-project` with >256 chars | Truncate name to 256 chars |
| Symlink in `cwd` | Canonicalized before detection |
| Permission denied on parent | Use original `cwd` as fallback |
| `tt project /some/path` | Output detection result without events |

## Acceptance Criteria

- [ ] `tt ingest` includes `project_root` and `project_name` in event output
- [ ] Detection algorithm follows innermost-marker-wins semantics
- [ ] Priority order at each level: marker file > VCS > package file
- [ ] Cache prevents redundant filesystem walks (100 entries, 5-min/1-min TTL)
- [ ] Timeout is best-effort (checked between operations)
- [ ] `tt project <path>` diagnostic command works
- [ ] `tt status` shows detection method
- [ ] `tt streams` shows grouping hint when multiple paths share a project
- [ ] Stream inference uses `project_root` when available
- [ ] Events without `project_root` fall back to `cwd` grouping
- [ ] `.tt-project` content is validated (256 char limit, no control chars)
- [ ] Symlinks canonicalized at start; not followed during walk
- [ ] Gitdir files only followed within same filesystem
