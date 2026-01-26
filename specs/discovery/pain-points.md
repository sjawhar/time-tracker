# Pain Points

_To be populated from user discovery._

## Format

Each pain point should include:
- **Description**: What is the problem?
- **Frequency**: How often does this occur?
- **Severity**: How much does this impact the user? (1-5)
- **Current Workaround**: How do users cope today?
- **Source**: Which interviews surfaced this?

---

## Discovered Pain Points

### Category: Visibility & Attribution

| ID | Pain Point | Frequency | Severity | Workaround |
|----|------------|-----------|----------|------------|
| P1 | Desktop trackers can't see inside SSH/tmux sessions | Always | 5 | Manual logging, retroactive fill |
| P2 | Session logs have random IDs, not task names | Always | 4 | LLM-based retroactive analysis |
| P3 | Can't distinguish human attention time from agent background time | Always | 4 | None |
| P4 | Interleaved/parallel work breaks linear "gap filling" model | Always | 5 | Accept lossy attribution |

### Category: Manual Overhead

| ID | Pain Point | Frequency | Severity | Workaround |
|----|------------|-----------|----------|------------|
| P5 | Manual start/stop too burdensome with 10+ concurrent contexts | Always | 5 | Stopped doing it |
| P6 | Weekly retroactive categorization takes too long | Weekly | 4 | Accept incomplete data |
| P7 | Hard to attribute time to projects when work spans multiple | Often | 3 | Arbitrary assignment |

### Category: Priority Alignment

| ID | Pain Point | Frequency | Severity | Workaround |
|----|------------|-----------|----------|------------|
| P8 | No view of "am I spending time on the right priorities?" | Always | 4 | Mental tracking |
| P9 | Can't tie time entries to Linear issues/todos | Always | 3 | Manual cross-reference |
| P10 | No insight into which tasks benefit from parallelization | Always | 3 | Intuition |

### Category: Technical Limitations

| ID | Pain Point | Frequency | Severity | Workaround |
|----|------------|-----------|----------|------------|
| P11 | Active pane ≠ current attention (dictation workflow) | Often | 3 | Accept inaccuracy |
| P12 | Toggl API has low rate limits | When syncing | 2 | Batch operations |
| P13 | No cross-device/cross-app integration | Always | 3 | Multiple data sources |

---

## Pain Point Ranking

**Critical (must solve in MVP):**
1. P1 - Desktop trackers can't see inside SSH/tmux (Severity 5)
2. P4 - Interleaved work breaks linear model (Severity 5)
3. P5 - Manual start/stop too burdensome (Severity 5)

**High (solve soon after MVP):**
4. P2 - Session logs have random IDs (Severity 4)
5. P3 - Can't distinguish human vs agent time (Severity 4)
6. P6 - Weekly categorization takes too long (Severity 4)
7. P8 - No priority alignment view (Severity 4)

**Medium (nice to have):**
8. P7 - Hard to attribute multi-project work (Severity 3)
9. P9 - Can't tie to Linear issues (Severity 3)
10. P10 - No parallelization insights (Severity 3)
11. P11 - Active pane ≠ attention (Severity 3)
12. P13 - No cross-device integration (Severity 3)

**Low (defer):**
13. P12 - Toggl API limits (Severity 2) - may not use Toggl
