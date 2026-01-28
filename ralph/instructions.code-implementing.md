# Code Implementing Instructions

You're building code based on a spec and implementation plan.

## One Task = One Commit

Each code task gets its own commit. Verify you're starting fresh.

## Workflow

1. **Read the plan**: Find the plan in `specs/plan.current.md`. Read the referenced spec.

2. **Write tests first**:
   - Write failing tests that define correct behavior
   - Cover happy path and edge cases
   - Run `cargo test` to confirm they fail

3. **Implement minimally**: Write the simplest code that passes tests. Resist:
   - Features not in the spec
   - Abstractions for future needs
   - Config options "just in case"

4. **Verify continuously**:
   ```bash
   cargo fmt --check && cargo clippy --all-targets && cargo test
   ```

5. **Measure performance**: If spec has perf requirements, benchmark before done.

6. **Review**: Launch agents:
   - **bug-finder**: Edge cases, error handling gaps
   - **code-simplifier**: Unnecessary complexity, dead code
   - **performance-engineer**: Bottlenecks, memory issues (especially remote CLI)

7. **Fix**: Address issues. Re-run tests.

8. **Live smoke testing**: You have access to a Kubernetes cluster (namespace: `researcher`).
   - Automated tests are required, but also smoke test with real pods
   - Spin up pods to test remote functionality, syncing, SSH, etc.
   - Label all pods: `app.kubernetes.io/name: time-tracker`
   - Clean up pods when done
   - **CRITICAL**: Only interact with pods you created. Never touch other K8s or AWS resources.

9. **Final check**: `cargo fmt && cargo clippy --all-targets && cargo test && cargo deny check`

10. **Commit**: `jj commit -m "Implement: [feature]"`

11. **Clean up**: Delete `specs/plan.current.md`

12. **Mark done**: Update the checkbox in `specs/plan.md`

## Core Principles

- **Tests define behavior**: If it's not tested, it doesn't work
- **Simple > clever**: Three clear lines beat one clever one
- **Fast startup**: Remote CLI <50ms, measure it
