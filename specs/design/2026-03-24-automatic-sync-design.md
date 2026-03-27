# Automatic Multi-Machine Event Sync

## Problem

tt collects activity events on 3+ machines (laptop, DevBox-MX, GhostWhisper/RPi) but syncing
them for unified time reports is painful:

1. **Sync is manual.** `tt sync devbox gpu-server` must be run by hand before every standup.
   No automation, no background process.
2. **Sync is hub-centric.** The laptop is the de facto hub because that's where the SQLite DB
   with all historical data lives. Running sync from a different machine loses that history.
3. **Sync requires SSH connectivity in the right direction.** The laptop pulls from remotes.
   If the laptop is behind NAT or offline, no sync happens. Remotes can't push.
4. **No "fleet" model.** Machines are cattle but treated like pets. You want to run `tt report`
   from any machine and get the same answer.

## Constraints

### Hard Constraints (non-negotiable)

- **Allocation algorithm is centralized.** `allocate_time()` requires ALL events from a time
  period in one place to compute focus timelines, agent session overlaps, and attention windows.
  Cannot be distributed across machines.
- **Data sensitivity.** Events contain `cwd` (filesystem paths), `project_path`, `user_prompts`,
  and `starting_prompt` — may include proprietary code, API keys, internal project names.
  Must be encrypted in transit. At-rest encryption strongly preferred.
- **Offline tolerance.** The laptop goes offline for hours (sleep, travel). The RPi may have
  intermittent connectivity. Events must accumulate locally and sync when connectivity returns.
- **Append-only events.** Events are immutable once written. IDs are deterministic
  (`{machine_uuid}:{source}:{type}:{timestamp}:{discriminator}`). This means merge = set union
  by ID. No conflict resolution needed for events themselves.

### Soft Constraints (preferences)

- Minimal operational overhead (no Kubernetes, no managed databases to babysit)
- Low cost (hobby-scale: 3 machines, ~2-3K events/day/machine, ~1-4 MB/day/machine)
- Rust-native preferred (avoid shelling out to external processes where possible)
- Should work with existing `rusqlite` codebase without full rewrite

## Current Architecture

```
Remote Machine (DevBox-MX, GhostWhisper)
├── tmux hooks → tt ingest → events.jsonl (append-only, 1MB rotation)
├── Claude sessions → ~/.claude/projects/*.jsonl
└── OpenCode sessions → ~/.local/share/opencode/opencode.db

     │ tt sync (SSH pull: tt export --after <last_id> | tt import)
     ▼

Laptop (Hub)
├── tt.db (SQLite) — all events, streams, agent_sessions, machines table
├── tt classify — LLM assigns events to streams
└── tt report — allocation algorithm computes time per stream
```

**Key properties of current sync:**
- Pull-based: hub initiates SSH to remotes, runs `tt export`, pipes to `tt import`
- Incremental: tracks `last_event_id` per remote machine in `machines` table
- Idempotent: `INSERT OR IGNORE` handles duplicates (deterministic event IDs)
- Stream assignments cleared on import (re-inferred after sync)
- Post-sync: index sessions + recompute allocation

## Data Characteristics

| Data Type | Size/Event | Volume/Day/Machine | Sensitive? |
|-----------|-----------|-------------------|------------|
| Tmux pane focus | ~600-800 bytes | 1400-2900 | Paths in `cwd` |
| AFK changes | ~300 bytes | 5-10 | No |
| Agent sessions | 10-60 KB | 5-20 | `user_prompts`, `project_path` |
| Agent tool uses | ~400 bytes | 50-200 | No |
| User messages | ~500 bytes | 10-50 | Content may be sensitive |
| **Total** | — | **~2-3K events** | **~1.3-4.2 MB/day** |

Monthly across 3 machines: **~120-380 MB**. Bandwidth is not a concern.

## Solutions Evaluated

### Category 1: SQLite Replication (7 solutions evaluated)

| Solution | Multi-Writer | Offline Writes | Status (Mar 2026) | Verdict |
|----------|-------------|----------------|-------------------|---------|
| LiteFS | ❌ | ❌ | ⚠️ Abandoned by Fly.io | Skip |
| Litestream | ❌ (backup only) | ✅ Primary | ✅ Active | Backup tool, not sync |
| cr-sqlite | ✅ CRDT | ✅ | ⚠️ Stalled (last release Jan 2024) | Risky |
| Turso/libSQL | ❌ Single primary | ⚠️ Beta | ✅ Active, good Rust SDK | Viable if offline writes not needed |
| rqlite | ❌ Raft leader | ❌ Needs quorum | ✅ Active | Requires HTTP rewrite, skip |
| mvsqlite | ✅ | ❌ Needs FoundationDB | ⚠️ Stalled | Way overkill, skip |
| Electric SQL | ✅ | ✅ | Pivoted to Postgres | No longer SQLite, skip |

**Conclusion:** No drop-in SQLite replication solution fits our multi-writer + offline + Rust
constraints. cr-sqlite is closest architecturally but development is stalled. Turso/libSQL is
viable only if we accept a single-primary model (writes fail when offline).

### Category 2: Cloud-Native Sync (7 approaches evaluated)

| Approach | Cost/Month | Complexity | Offline | Rust SDK | Verdict |
|----------|-----------|-----------|---------|----------|---------|
| S3 per-machine files | ~$0 | Low | Buffer+upload | ✅ aws-sdk-s3 | **Best fit** |
| DynamoDB | $0 (free tier) | Low-Med | Buffer+flush | ✅ aws-sdk-dynamodb | Good alternative |
| SQS/SNS | $0 | High | 14-day queue | ⚠️ Quirks | Overcomplicated |
| AWS IoT Core | ~$0.05 | Very High | MQTT QoS 1 | ❌ No SDK | Overkill |
| Syncthing | $0 | Medium | ✅ Native | N/A (daemon) | No-cloud option |
| sqlite3_rsync | $0 | Low | SSH-dependent | N/A (binary) | Upgrade to current |
| SSH push | $0 | Low | Buffer+push | ✅ openssh | Complement to any |

**Conclusion:** S3 per-machine event files is the simplest, cheapest cloud option. Each machine
uploads batch files to its own S3 prefix. `tt sync` pulls from S3 and merges. DynamoDB is a
good alternative if queryable cloud storage is wanted (future dashboard).

### Category 3: Mesh/P2P Sync (7 solutions evaluated)

| Solution | Rust Maturity | Effort | Encryption | Offline | Verdict |
|----------|-------------|--------|-----------|---------|---------|
| crdts (GSet) | ✅ Stable | High (no transport) | DIY | ✅ | Building blocks only |
| Automerge-rs | ✅ Stable | Medium (sync built-in) | DIY | ✅ | Battle-tested, BYO transport |
| Loro | ✅ Active | Medium | DIY | ✅ | Newer, great perf |
| Hypercore-rs | ⚠️ Incomplete | High | ✅ Noise | ✅ | Pre-v1.0, skip |
| **Iroh + iroh-docs** | ✅ Active (pre-1.0) | **Low** | **✅ QUIC/TLS** | ✅ | **Best P2P option** |
| Tailscale + custom | ✅ (Tailscale) | Medium | ✅ WireGuard | ✅ | Pragmatic path |
| Matrix | ✅ Production | High (homeserver) | ✅ Olm | ✅ | Massive overkill |

**Conclusion:** Iroh is the standout P2P option — Rust-native, batteries-included (networking +
CRDT sync + encryption). Tailscale + Automerge is the pragmatic alternative. Both are viable.

---

## Proposed Approaches

### Approach A: S3 Event Hub (Recommended)

**Architecture:** Each machine pushes event batches to its own S3 prefix. Any machine can pull
all events from S3 and build a complete local database.

```
Laptop ──push──► s3://tt-events/laptop/20260324T143022Z.jsonl
DevBox ──push──► s3://tt-events/devbox/20260324T180011Z.jsonl
RPi    ──push──► s3://tt-events/rpi/20260324T220033Z.jsonl
                           │
                   ◄──pull── tt sync (from ANY machine)
```

**How it works:**
1. Each machine runs `tt push` on a timer (cron/systemd, every 5-15 min)
2. `tt push` reads events.jsonl + sessions since last push, uploads as a batch file to S3
3. `tt sync` (or `tt pull`) downloads all machine prefixes from S3, merges into local SQLite
4. Merge is idempotent: event IDs are deterministic, `INSERT OR IGNORE` handles duplicates
5. Any machine can run sync — no designated hub

**Security:**
- S3 server-side encryption (SSE-S3 or SSE-KMS) for at-rest
- HTTPS for in-transit (default with AWS SDK)
- IAM credentials on each machine (one IAM user, scoped to the bucket)
- Sensitive fields (user_prompts, cwd) encrypted before upload with a shared symmetric key

**Offline handling:**
- Events accumulate locally in events.jsonl (already works this way)
- `tt push` retries on failure, tracks last-pushed position
- When laptop wakes from sleep, push + sync run automatically

**Pros:**
- Simplest architecture. No servers, no daemons, no consensus.
- $0/month at hobby scale (well within S3 free tier)
- Any machine can be the hub — true "cattle" model
- Great Rust SDK (`aws-sdk-s3`, mature, async/tokio)
- Natural upgrade path: add DynamoDB later for queryable data, add Lambda for automation

**Cons:**
- Requires AWS account + IAM setup (one-time)
- AWS credentials on each machine (manage with `~/.aws/credentials`)
- Not real-time (batch interval of 5-15 min, could go to 1 min)
- Requires internet connectivity for push (no LAN-only fallback)
- Adds tokio dependency for async AWS SDK (but `#[tokio::main]` already stubbed)

**Estimated effort:** 2-3 days. New `tt push` command, modify `tt sync` to read from S3,
cron/systemd setup on each machine.

---

### Approach B: Iroh P2P Mesh

**Architecture:** Each machine runs an iroh node. Events sync automatically via iroh-docs
(CRDT key-value store with range-based set reconciliation).

```
Laptop ◄──iroh-docs──► DevBox
   ▲                      ▲
   └──────iroh-docs───────┘
              │
          GhostWhisper
```

**How it works:**
1. Each machine runs an iroh endpoint (background process or on-demand)
2. Events written to a shared iroh-docs "document" (namespace), keyed by event ID
3. When machines connect (same network or via relay), iroh-docs automatically reconciles
4. Each machine has a complete local copy of all events at all times
5. Allocation runs locally on any machine — all events are already there

**Security:**
- All connections authenticated + encrypted via QUIC/TLS 1.3
- Node identity = Ed25519 keypair
- Sensitive payloads encrypted before storing in iroh-docs (application-level)

**Offline handling:**
- Native. Each machine writes locally. Sync happens when peers reconnect.
- n0 runs public relay servers for cross-network connectivity
- Can self-host a relay on the always-on DevBox

**Pros:**
- True mesh — no central point of failure, no cloud dependency
- Encryption built-in (QUIC/TLS 1.3 for transport)
- Handles NAT traversal, hole-punching, relay fallback
- Rust-native (the whole stack is Rust)
- Real-time sync when connected (not batch-based)
- Elegant: event log maps directly to iroh-docs entries

**Cons:**
- Pre-1.0 (v0.97.0 as of Mar 2026) — expect API churn
- Requires async migration (iroh is tokio-only, current codebase is sync)
- Need a relay for cross-network sync (n0 public relays, or self-host)
- More complex operationally (iroh daemon/process management)
- Newer, less battle-tested than S3
- Larger dependency footprint (QUIC stack, crypto, networking)

**Estimated effort:** 5-7 days. Async migration for sync commands, iroh integration,
key management, relay setup, testing across network topologies.

---

### Approach C: Tailscale + Enhanced SSH Sync (Evolutionary)

**Architecture:** Use Tailscale as the networking layer (WireGuard mesh VPN). Enhance existing
`tt sync` with push model and automation. Lowest-risk path.

```
             Tailscale Mesh (WireGuard)
         ┌──────────────────────────────────┐
         │                                  │
Laptop ◄─┤── tt sync (SSH over Tailscale) ──├─► DevBox
(100.x)  │                                  │   (100.x)
         │          GhostWhisper            │
         │            (100.x)               │
         └──────────────────────────────────┘
```

**How it works:**
1. Install Tailscale on all 3 machines (free tier covers this)
2. Machines get stable `100.x.x.x` IPs + MagicDNS hostnames
3. Enhance `tt sync` to support both push and pull:
   - `tt sync --push devbox` — push my events to devbox
   - `tt sync --pull devbox` — pull devbox events to me (existing behavior)
4. Add systemd timer: on reconnect, push events to all known peers
5. Any machine can be the hub — just pull from all others
6. Add Litestream for S3 backup safety on the primary machine

**Security:**
- WireGuard encryption for all traffic (ChaCha20-Poly1305)
- SSH authentication on top (already in place)
- No data leaves the Tailscale mesh (no cloud storage of events)

**Offline handling:**
- Events accumulate locally (already works)
- Tailscale handles reconnection automatically
- Systemd timer triggers sync on network state change

**Pros:**
- Minimal code changes — extends existing `tt sync` command
- Battle-tested security (WireGuard, Tailscale used by millions)
- No cloud storage of sensitive data (events stay on your machines)
- Works through NAT, firewalls, mobile networks
- Free tier covers 3 machines (100 devices)
- Fastest to implement

**Cons:**
- Still SSH-based — slower than direct S3 or P2P
- Tailscale is a centralized coordination service (though data is P2P)
- One machine still needs to be "the hub" for unified view (unless all push to all)
- No cloud backup unless you add Litestream separately
- Tailscale daemon is an external dependency on each machine
- All-to-all push with 3+ machines creates O(n²) sync traffic (manageable at 3 machines)

**Estimated effort:** 1-2 days. Add push mode to `tt sync`, Tailscale install, systemd timers.

---

## Comparison

| Criterion | A: S3 Hub | B: Iroh Mesh | C: Tailscale+SSH |
|-----------|-----------|-------------|-----------------|
| **Complexity** | Low | Medium-High | Very Low |
| **Cost** | $0 | $0 | $0 |
| **Implementation time** | 2-3 days | 5-7 days | 1-2 days |
| **"Run from any machine"** | ✅ Full | ✅ Full | ⚠️ Requires push-to-all |
| **Offline tolerance** | ✅ Buffer+push | ✅ Native CRDT | ✅ Buffer+push |
| **Encryption (transit)** | ✅ HTTPS | ✅ QUIC/TLS 1.3 | ✅ WireGuard |
| **Encryption (at-rest)** | ✅ SSE + app-level | ⚠️ App-level only | ❌ None (local disk) |
| **Data stays on your machines** | ❌ S3 copy | ✅ Yes | ✅ Yes |
| **Real-time sync** | ⚠️ Batch (1-15 min) | ✅ Yes | ⚠️ Timer-based |
| **External dependencies** | AWS SDK | iroh (pre-1.0) | Tailscale daemon |
| **Maturity** | ✅ Production | ⚠️ Pre-1.0 | ✅ Production |
| **Rust integration** | ✅ aws-sdk-s3 | ✅ Native Rust | ✅ SSH (existing) |
| **Future extensibility** | DynamoDB, Lambda | Custom protocols | Limited |

## Recommendation

**Start with Approach A (S3 Hub).** It's the best balance of simplicity, correctness, and
the "cattle fleet" model you want:

1. Any machine can push events to S3. Any machine can pull and build a complete local DB.
2. No designated hub — the laptop is no longer special.
3. S3 is virtually free, highly durable, and you're comfortable with AWS.
4. The Rust AWS SDK is mature. Implementation is straightforward.
5. Natural upgrade path: add DynamoDB for queryable data, Lambda for automation.

**Approach C (Tailscale+SSH) is the fast lane** if you want something working this week with
minimal disruption to the existing codebase.

**Approach B (Iroh) is the most elegant long-term** but carries pre-1.0 risk and larger
implementation scope. Worth revisiting when iroh reaches 1.0.

**A possible hybrid:** Start with C (Tailscale for networking, immediate improvement to sync),
then migrate to A (S3 as the durable hub) for the "run from any machine" model. This gives
you quick wins now and the right architecture later.

---

## Open Questions for Discussion

1. **How critical is "run from any machine"?** If one machine being the hub is acceptable
   (just with better automation), Approach C is fastest. If true fleet parity is the goal,
   Approach A is the way.

2. **How sensitive is the data, really?** You mentioned IP concerns. Is this "encrypt before
   leaving my machines" sensitive, or "encrypt at rest on S3 with KMS" sufficient? This
   affects whether cloud storage (A) is acceptable or if data must stay on your machines (B/C).

3. **Do you already have Tailscale?** If it's already installed on all 3 machines, Approach C
   is a 1-day win. If not, the install overhead is similar to AWS credential setup.

4. **Real-time vs. batch sync?** For standup prep, batch sync every 5-15 minutes is fine.
   Is there a use case where you need events synced within seconds?

5. **Session data handling.** Agent sessions (with sensitive user_prompts) are 10-60 KB each.
   Should these sync with events, or should session scanning always happen locally on each
   machine and only metadata (session_id, timestamps, message counts) sync?

6. **Stream assignments.** Currently cleared on import and re-inferred. Should user-assigned
   streams sync across machines? This would require tracking assignment provenance.

7. **What's the RPi's role?** Is GhostWhisper a development machine (generating events) or
   could it serve as an always-on hub/relay?

8. **Tolerance for new dependencies?** The S3 approach adds `aws-sdk-s3` + `aws-config` +
   tokio (for async). The iroh approach adds the entire iroh stack. The Tailscale approach
   adds almost nothing code-wise. What's your comfort level?
