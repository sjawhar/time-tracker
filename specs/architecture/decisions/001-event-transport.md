# ADR-001: Event Transport from Remote to Local

## Status

**Accepted**

## Context

The time tracker captures events on remote dev servers (tmux hooks, Claude session logs) but stores them in SQLite on the local machine. We need a mechanism to transport events from remote to local.

**Constraints:**
- Multiple remote servers may exist
- Network connectivity is intermittent (SSH sessions may disconnect)
- Events must not be lost
- Latency of minutes is acceptable (not real-time requirements)
- Minimal infrastructure overhead preferred

## Options

### Option A: Pull-based (SSH polling)

Local machine periodically SSHs to each remote and pulls new events.

```
Local: cron/daemon runs `tt sync remote1 remote2 ...`
  └── SSH to remote1 → read events since last sync → store locally
  └── SSH to remote2 → read events since last sync → store locally
```

**Implementation:**
- Remote stores events in JSONL file or SQLite
- Remote tracks nothing; local tracks `last_sync_position` per remote
- Sync command: `ssh remote "tt events --since=$POSITION --format=jsonl"` → parse → insert locally

**Pros:**
- Simple implementation — just SSH + read
- Works with any remote that has SSH access
- No additional infrastructure
- Clear security model (SSH keys)
- Events buffered on remote if local is offline

**Cons:**
- Not real-time (polling interval = latency)
- SSH connection overhead per remote per poll
- Need to track sync position per remote
- Scales linearly with number of remotes

### Option B: Shared Storage (S3/Syncthing)

Events written to shared storage that both remote and local can access.

```
Remote: tt ingest → write to shared storage
Local: watcher/poller reads from shared storage → insert to SQLite
```

**Sub-options:**

**B1: S3 bucket**
- Remote writes events as objects: `s3://bucket/events/{remote}/{timestamp}.jsonl`
- Local reads new objects periodically or via S3 event notification
- Pros: Reliable, cheap (~$0.01/month), scales naturally
- Cons: Requires AWS account, internet dependency, ~100ms write latency

**B2: Syncthing**
- Events written to `~/time-tracker-events/` folder synced by Syncthing
- Pros: Peer-to-peer, no cloud dependency, near-instant sync when connected
- Cons: Syncthing must run on all machines, potential conflict issues

**B3: rsync daemon**
- Events written to local file on remote
- Periodic rsync pulls to local
- Pros: Simple, reliable
- Cons: Essentially pull-based with extra steps

**General shared storage pros:**
- Decouples remote and local — no direct connection needed
- Scales to many remotes without linear SSH connections
- Can add new remotes without local configuration

**General shared storage cons:**
- Additional infrastructure to set up and maintain
- Another potential failure point
- More complex debugging (is the issue remote, storage, or local?)

## Comparison

| Factor | Pull (SSH) | Shared (S3) | Shared (Syncthing) |
|--------|------------|-------------|---------------------|
| Setup complexity | Low | Medium | Medium |
| Infrastructure | None | AWS account | Syncthing on all machines |
| Real-time latency | Poll interval | ~1s (with notifications) | ~1s |
| Offline resilience | Good | Good | Good |
| Multiple remotes | O(n) connections | O(1) | O(1) |
| Security | SSH keys | IAM | Syncthing keys |
| Debugging | Simple | Medium | Medium |

## Recommendation

**For prototype: Pull-based (Option A)**

Rationale:
1. Simplest to implement — get data flowing first
2. No infrastructure to set up
3. We only have 1-2 remotes initially; scaling isn't a concern yet
4. Can switch to shared storage later if needed (events are portable)

**For MVP: Re-evaluate**

If we find pull-based is too slow or we have many remotes, consider S3. Syncthing adds operational complexity that may not be worth it.

## Implementation Notes

**Pull-based prototype:**
1. Remote: `tt ingest` appends to `~/.time-tracker/events.jsonl`
2. Remote: `tt export` reads events.jsonl + parses Claude logs (with manifest for incremental parsing)
3. Local: `tt sync user@remote` runs:
   ```bash
   ssh user@remote "tt export --since=$LAST_UUID" | tt import --source=remote
   ```
4. Sync position tracked by last imported UUID
5. Deterministic event IDs ensure idempotent imports

**Migration path to S3:**
- Change remote to write to S3 instead of local file
- Change local to read from S3 instead of SSH
- Event format stays the same — no data migration needed

## Decision

**Pull-based sync via SSH (Option A)**

Simple, no infrastructure, sufficient for 1-2 remotes. Re-evaluate if scaling becomes a concern.
