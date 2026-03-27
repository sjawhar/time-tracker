# Automatic Multi-Machine Sync: Research & Analysis

**Date**: 2026-03-24
**Status**: Research complete, awaiting discussion
**Context**: ADR-001 chose SSH pull sync for prototype. Now at 3 machines (laptop, DevBox-MX, GhostWhisper) and the pain is real.

---

## Problem Statement

Daily standup sync takes too long. Must run from the laptop because that's where all data has been ingested. Want any machine to be a first-class participant — "cattle, not pets" — sharing one logical dataset.

### User Requirements (extracted from conversation)

1. **Automatic sync** — when laptop comes online, accumulated events sync without manual `tt sync`
2. **Any-machine access** — run reports, classify, etc. from any machine
3. **Fleet mentality** — all machines share one data source
4. **Security** — Claude/OpenCode session data can contain IP; must be encrypted appropriately

### Current Pain Points

- `tt sync` is manual SSH pull — must remember to run it
- Must run from laptop (single-machine assumption in the workflow)
- 3 machines × SSH = slow sequential sync
- If laptop is offline, can't sync at all
- No automatic retry or catch-up mechanism

---

## Current Architecture (for context)

```
Tmux hooks → events.jsonl (per machine)
Claude/OpenCode sessions → parsed on-demand during export
tt export → JSONL to stdout (incremental via --after)
tt sync → SSH to each remote, run tt export, pipe to tt import
tt import → INSERT OR IGNORE into SQLite (idempotent)
```

**Key properties that make this tractable:**
- Events are **append-only** — no updates, no deletes in normal operation
- Event IDs are **globally unique** — `{machine_uuid}:{source}:{type}:{timestamp}:{discriminator}`
- Merge is **trivially correct** — `INSERT OR IGNORE` = set union, no conflict resolution needed
- Data is **small** — 64 KB to 1.3 MB/day/machine, ~10K events/day across all machines
- Each machine only writes its own events — **no cross-machine writes**

These properties mean we do NOT need CRDTs, consensus protocols, or any distributed systems complexity. The "hard" distributed data problems simply don't apply.

---

## Approaches

### Approach A: File Sync Layer (Syncthing)

**The idea**: Use per-machine event files. Syncthing handles replication transparently.

**How it works**:
1. Each machine writes to `events-{machine_id}.jsonl` (already has machine-scoped IDs)
2. Syncthing watches `~/.local/share/time-tracker/` and syncs across all machines
3. `tt import` reads all peer event files and merges into local SQLite
4. On startup or periodic schedule, each machine imports peer events

**What changes in `tt`**:
- Split events.jsonl into per-machine files (small refactor)
- Add `tt import --from-peers` that scans for peer event files
- Optionally: systemd timer or cron for periodic local import
- Remove or simplify `tt sync` (Syncthing replaces SSH transport)

**DO NOT sync SQLite directly** — Syncthing + open SQLite = database corruption. Only sync the JSONL event files and let each machine maintain its own SQLite.

| Pro | Con |
|-----|-----|
| Near-zero code changes | Syncthing must run on all 3 machines |
| Battle-tested sync (81K GitHub stars) | ~1-30s sync latency (fine for time tracking) |
| Excellent offline support | SQLite stays per-machine (must import after sync) |
| Peer-to-peer, no cloud needed | Syncthing is another process to manage |
| Auto-reconnects and catches up | JSONL files grow unbounded (need rotation strategy) |
| Encrypted in transit (TLS) | At-rest encryption is your responsibility |
| Works over LAN, Tailscale, or internet | |

**Effort**: ~4 hours (per-machine files + import-from-peers command)
**Ongoing ops**: Install/configure Syncthing once on each machine

---

### Approach B: Cloud Event Store (AWS)

**The idea**: S3 or DynamoDB as the central event store. Each machine pushes when online, pulls on demand.

#### Option B1: S3 (Simplest cloud option)

```
Machine A → batch events → PUT s3://tt-events/{machine_id}/{date}.jsonl
Machine B → GET s3://tt-events/* → merge into local SQLite
```

- Batch events into daily JSONL files, push every N minutes
- On sync: download all peer files newer than last-seen timestamp
- Client-side encryption with `age` crate before upload (zero-knowledge)
- **Cost: effectively $0** (fits in S3 free tier, then ~$0.01/month)

#### Option B2: DynamoDB (Richer querying)

- Each event is a DynamoDB item: `PK=machine_id, SK=timestamp#event_id`
- Background thread flushes local SQLite buffer to DynamoDB when online
- On startup: pull events newer than `last_sync_timestamp` per machine
- **Cost: $0** (free tier covers 2.5M writes + 2.5M reads/month; you need ~300K/month)

| Pro | Con |
|-----|-----|
| Any machine syncs from anywhere | Requires internet connectivity for sync |
| No peer-to-peer networking needed | AWS account setup + IAM config |
| Durable cloud backup for free | Vendor lock-in (mild — just JSONL files) |
| Client-side encryption = zero-knowledge | Additional latency for Raspberry Pi |
| Scales to any number of machines | More code: AWS SDK integration (~200 LoC) |
| Works even if machines never see each other | Credential management on 3 machines |

**Effort**: ~2-3 days (AWS SDK integration, encryption, background push/pull)
**Ongoing ops**: Minimal (AWS free tier, no servers to manage)

---

### Approach C: Self-Hosted Event Relay (Tailscale + `tt serve`)

**The idea**: Each machine runs a tiny HTTP server. Peers sync via HTTP over Tailscale mesh.

```
Machine A ←──HTTP──→ Machine B ←──HTTP──→ Machine C
           (all connected via Tailscale WireGuard mesh)
```

**How it works**:
1. `tt serve` starts a lightweight axum server bound to Tailscale interface
2. Exposes: `GET /events?since={timestamp}` → returns JSONL
3. `tt sync` becomes an HTTP client: polls each peer's `/events` endpoint
4. Background sync daemon (systemd timer) runs `tt sync` every 5 minutes
5. Tailscale handles NAT traversal, encryption (WireGuard), device identity

**What this evolves from current design**: The SSH-based sync already works. This replaces SSH with HTTP (simpler, no shell escaping) and Tailscale with direct SSH (no key management, auto NAT traversal).

| Pro | Con |
|-----|-----|
| Zero cloud cost | Tailscale must run on all machines |
| WireGuard encryption (excellent security) | Requires `tt serve` daemon on each machine |
| Auto NAT traversal | More code than Syncthing approach |
| Tailscale free tier covers 100 devices | Machines must be online simultaneously for sync |
| Natural evolution of current SSH sync | Tailscale coordination server sees device metadata |
| `tt serve` also enables future TUI/API use | |

**Effort**: ~1-2 days (axum server, HTTP sync client, systemd unit)
**Ongoing ops**: Tailscale free tier, no servers

---

### Approach D: Hybrid (Local-first + Cloud backup)

**The idea**: Combine A or C for fast peer sync with B for cloud durability.

```
Fast path:  Machine A ←→ Syncthing/Tailscale ←→ Machine B
Durable:    All machines → S3 (encrypted backup, async)
Recovery:   New machine → pull from S3 → caught up
```

This gives you:
- Fast peer sync when machines are on the same network
- Cloud backup for durability and "sync from anywhere"
- New machine onboarding: just pull from S3
- No single point of failure

| Pro | Con |
|-----|-----|
| Best of both worlds | Most complex to implement |
| Works offline (local sync) AND remotely (cloud) | Two sync paths to maintain |
| Cloud backup = disaster recovery | May be over-engineering for 3 machines |
| New machine setup is trivial | |

**Effort**: ~3-5 days (combines A/C + B)
**Ongoing ops**: Syncthing/Tailscale + AWS free tier

---

## Approaches I Investigated and Rejected

### cr-sqlite (CRDT SQLite extension)
Architecturally the best fit for true peer-to-peer SQLite sync. **Rejected because**: last release January 2024 (14+ months stale), pre-1.0, 2.5x insert overhead, and your append-only data doesn't need CRDT conflict resolution. The simpler approaches achieve the same result.

### rqlite / dqlite (Raft consensus SQLite)
**Rejected because**: Raft requires quorum (2 of 3 nodes) for writes. If 2 machines are offline, writes fail. Fundamentally incompatible with offline-first, intermittent-connectivity requirement.

### LiteFS (FUSE-based distributed SQLite)
**Rejected because**: Single-primary (same as Litestream), requires FUSE (not available everywhere), Fly.io deprioritized development, LiteFS Cloud was sunset October 2024.

### Electric SQL
**Rejected because**: Postgres-centric, no Rust client, designed for web/mobile apps. Wrong tool entirely.

### Blockchain
You called it — terrible idea for this. Consensus overhead, storage bloat, and complexity for a problem that has no adversarial trust requirement. Your machines trust each other; you just need reliable data movement.

### NATS JetStream
Viable as a transport layer but requires running a NATS server somewhere. For 3 machines with append-only data, it's more infrastructure than needed. Would make sense at 10+ machines.

---

## Comparison Matrix

| Factor | A: Syncthing | B: Cloud (S3) | C: Tailscale+HTTP | D: Hybrid |
|--------|:---:|:---:|:---:|:---:|
| Code changes | Minimal | Medium | Medium | High |
| Setup effort | 4 hrs | 2-3 days | 1-2 days | 3-5 days |
| Ongoing ops | Low | Minimal | Low | Low |
| Works offline | ✅ | ❌ (needs internet) | ✅ (needs peer) | ✅ |
| Any-machine access | ✅ | ✅ | ✅ | ✅ |
| Auto-sync on reconnect | ✅ | ✅ (with daemon) | ✅ (with daemon) | ✅ |
| Cloud backup | ❌ | ✅ | ❌ | ✅ |
| New machine onboarding | Pull from peer | Pull from S3 | Pull from peer | Pull from S3 |
| Security (transit) | TLS | TLS + client encryption | WireGuard | Both |
| Security (at rest) | Your responsibility | Client-side `age` | Your responsibility | Client-side `age` |
| Cost | $0 | $0 (free tier) | $0 | $0 |
| External dependencies | Syncthing daemon | AWS SDK, account | Tailscale, axum | All of the above |

---

## Recommendation

**Start with Approach A (Syncthing)**, evolve to D (Hybrid) if cloud backup becomes important.

### Why A first:

1. **Lowest risk, fastest value** — 4 hours of work, solves the core pain immediately
2. **No new Rust dependencies** — just restructure event files and add a peer-import command
3. **Syncthing is battle-tested** — 81K stars, handles reconnection/NAT/encryption out of the box
4. **Matches your data model perfectly** — per-machine JSONL files, no conflicts, trivial merge
5. **Non-destructive** — keep `tt sync` SSH as fallback; Syncthing is additive

### What "A done right" looks like:

1. `tt ingest` writes to `events-{machine_id}.jsonl` (not a shared file)
2. Syncthing syncs `~/.local/share/time-tracker/events-*.jsonl` across machines
3. `tt refresh` (new command) imports all peer event files into local SQLite
4. systemd timer runs `tt refresh` every 5 minutes (or on Syncthing file-change event)
5. `tt classify`, `tt report`, etc. all work locally on merged SQLite — any machine, any time
6. Claude/OpenCode session scanning runs locally per-machine (sessions stay local, only parsed events sync)

### When to evolve to D:

- If you want cloud backup / disaster recovery
- If you add a machine that can't run Syncthing (e.g., ephemeral cloud instance)
- If you want to sync from a machine that has never been peered with Syncthing

### What stays the same regardless of approach:

- Event ID format (`{machine_uuid}:...`) — already globally unique
- `INSERT OR IGNORE` merge strategy — already idempotent
- SQLite stays local per machine — never sync the database file
- Classification and time allocation happen locally after merge

---

## Open Questions for Discussion

1. **Session data sensitivity**: Currently user prompts (truncated to 2000 bytes, max 5) and session summaries are synced. Is that acceptable, or should we encrypt these fields specifically? Or exclude them from sync entirely?

2. **"Run from any machine" scope**: Does this mean full capability (classify, report, tag) from any machine? Or just "all events are available" with classification done from a primary machine? The difference: if classification is per-machine, streams may diverge. If it's synced, we need to sync stream assignments too.

3. **Existing Syncthing/Tailscale setup**: Do you already have Syncthing or Tailscale on these machines? That would affect the recommended approach.

4. **Agent session data locality**: Claude/OpenCode sessions live on the machine where they ran. Currently `tt export` parses them on-demand. Should the parsed session metadata sync across machines, or should each machine only know about its own sessions? (Current behavior: sync sends parsed events + session metadata.)

5. **Standup workflow**: You mentioned standup sync takes forever. Is the bottleneck (a) the SSH connection time, (b) the data transfer volume, (c) the session parsing time, or (d) the manual "remember to run it" overhead? This affects whether we optimize the sync mechanism or just automate the existing one.

6. **JSONL file growth**: events.jsonl currently rotates at 1MB. With per-machine files synced via Syncthing, should we rotate more aggressively? Or archive old events to a separate file that Syncthing ignores?
