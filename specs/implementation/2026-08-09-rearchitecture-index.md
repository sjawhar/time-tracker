# Rearchitecture: Index and Sequencing

**Scope:** The whole rearchitecture discussed on 2026-08-09 — allocation, the classifier's async shape, and the classifier's read path. Three documents because the skill requires each plan to produce working, testable software on its own, **not** because any part is deferred. All three are written; execute all three.

**One sentence:** The codebase runs a Rust workload like a scripting language — everything materialized into memory, one core, and an async HTTP stack blocked into a sync trait and then re-parallelized with OS threads.

## The plans

| # | Plan | What it fixes | Measured payoff |
|---|---|---|---|
| 1 | [`2026-08-09-allocation-streaming-plan.md`](2026-08-09-allocation-streaming-plan.md) | `tt recompute` materializes 2,738,805 events **twice** and folds them on one core | **250 s → target seconds; 8.9 GB → target <1 GB; 1 core → 24** |
| 2 | [`2026-08-09-async-classifier-plan.md`](2026-08-09-async-classifier-plan.md) | Async HTTP blocked into a sync trait, re-parallelized with OS threads, called from an async daemon | **None, and it says so.** Parity at ~7 real sessions/min. Removes a second runtime and a thread stack per call |
| 3 | [`2026-08-09-read-pool-plan.md`](2026-08-09-read-pool-plan.md) | Every concurrent worker serializes session fetches on one `Mutex<Database>` while WAL allows unlimited readers | **Unknown — Task 1 measures it and Task 1 Step 4 is an explicit exit** |

## Recommended order, and why

**Plan 1 first.** It is the only one with a hard measured number (340× against the equivalent SQLite aggregate), it is entirely self-contained in the allocation path, and it touches nothing the other two touch. It also has the highest value per unit of risk: the algorithm is *already* a single forward pass, so it is a refactor rather than a rewrite.

**Plan 3 second, or never.** It is small, independent, and its first task decides whether it is worth doing at all. Running it early means the answer is known before Plan 2 changes the concurrency mechanism around it. If the measurement says the contention is a rounding error against a 15–90 s model call, the correct outcome is a doc comment explaining why one connection is enough, and closing the plan.

**Plan 2 last.** It is the largest, the riskiest, and — stated plainly in its own header — **buys no throughput today**. The provider rate-limits above `CLASSIFY_CONCURRENCY = 8`; 16 was measured at +18% with 2 × 429 and 6 × 529 and reverted. Plan 2 is an architecture fix, not a performance fix, and it should not displace a plan that is both.

They are independent: none consumes an interface produced by another, so this order is a recommendation about risk, not a dependency graph.

## Shared invariants

Every plan carries these, and each states them itself:

- **Direct time must not move.** `2026-07-20` prints `Direct time: 16h 23m`; `2026-07-13..2026-07-20` prints `74h 20m`. Checked before and after each task against a copy of the live database.
- **Delegated time sums across parallel agents and routinely exceeds wall clock.** Never union, cap, or clamp it.
- **Classifier writes stay serial on one connection.** `stream_named` is a non-transactional find-and-insert; two concurrent copies re-create the duplicate-stream failure that cost 55 renames, 5 merges and 9 dissolves. Plan 2 notes that this guarantee stops being compiler-enforced when threads become futures, which is the single thing a reviewer should check by hand.
- Zero clippy warnings (CI is `-D warnings`), `cargo fmt` clean, `unsafe_code` denied.
- **jj, not git.**

## What is deliberately not here

- **Splitting `allocation.rs` (2,490 lines) or `tt-db/src/lib.rs` (10,400 lines).** Both are far past any size ceiling and both are the house pattern. Splitting either is its own change with its own review, and doing it inside a performance refactor would make the diff unreviewable.
- **Raising `CLASSIFY_CONCURRENCY`.** Measured and reverted; the reasoning is recorded on the constant.
- **Anything about attribution rules.** No plan here changes what a stream is, how work is placed, or what counts as attention.

## The thing worth remembering

From Plan 1's `AGENTS.md` entry: **the slowness had become load-bearing.** `tt sync` was made 3.6 min → 18.6 s by *deleting* its recompute call, that deletion is now a rule with its own regression test, and `tt streams` labels its totals with their age rather than refreshing them. A 250-second function shaped three other designs before anyone treated it as a defect.

That is the pattern these three plans exist to break, and it is the reason to run Plan 1 even though the product currently works: designing around a fixable 340× is how a codebase stops being able to use the language it is written in.
