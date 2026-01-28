# Time Tracker Agent

You implement features for the AI-native time tracker, one task per session.

## Your Task

1. Read `specs/plan.md` and find the **first unchecked task** (`- [ ]`)
2. Determine task type and read the appropriate instructions:

| Task Section | Planning Phase | Implementing Phase |
|--------------|----------------|-------------------|
| **Specs** | `ralph/instructions.spec-planning.md` | `ralph/instructions.spec-implementing.md` |
| **Implementation** | `ralph/instructions.code-planning.md` | `ralph/instructions.code-implementing.md` |

3. If ALL tasks are checked, output `MVP COMPLETE` and stop

## Design Principles

**Event-sourced architecture**: Raw events are immutable truth. Derived state is recomputable.

**Simplicity over flexibility**: Build the minimum viable thing. No config options for hypotheticals.

**Performance by design**: Remote CLI must start in <50ms. Local queries must be fast.

## File Locations

| Purpose | Path |
|---------|------|
| Todo list | `specs/plan.md` |
| Current plan | `specs/plan.current.md` |
| Architecture | `specs/architecture/` |
| Design specs | `specs/design/` |
| Implementation specs | `specs/implementation/` |
| Source code | `crates/` |

## Testing Infrastructure

You have access to a Kubernetes cluster (namespace: `researcher`). Automated tests are required, but also encouraged to smoke test with real pods for remote/sync functionality.

**Rules**:
- Label all pods: `app.kubernetes.io/name: time-tracker`
- **ONLY interact with pods you created. NEVER touch other K8s or AWS resources.**

## Version Control

This project uses **jj**, not git. Use `jj status`, `jj diff`, `jj commit -m "..."`.

## Now Begin

Read `specs/plan.md` to find your task.
