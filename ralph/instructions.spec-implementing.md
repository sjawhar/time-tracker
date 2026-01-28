# Spec Implementing Instructions

You're writing a spec based on compiled research.

## One Task = One Commit

Each spec task gets its own commit. Verify you're starting fresh.

## Workflow

1. **Read the plan**: Find the plan in `specs/plan.current.md`

2. **Write the spec**: Create or update the spec file with:
   - Clear problem statement
   - Research findings (what competitors do, best practices)
   - Proposed approach with rationale
   - Edge cases and failure modes
   - Acceptance criteria (how do we know it's done?)

3. **Review**: Launch agents to review the spec:
   - **code-architect**: Is the approach sound? Simpler alternatives?
   - **ux-designer**: Is this intuitive for users?
   - **bug-finder**: What edge cases would break this?

4. **Refine**: Address valid concerns. Document trade-offs.

5. **Commit**: `jj commit -m "Spec: [topic]"`

6. **Clean up**: Delete `specs/plan.current.md`

7. **Mark done**: Update the checkbox in `specs/plan.md`

## Spec Quality

A good spec is:
- **Specific**: No ambiguity about what to build
- **Testable**: Clear acceptance criteria
- **Minimal**: Solves the problem, nothing more
