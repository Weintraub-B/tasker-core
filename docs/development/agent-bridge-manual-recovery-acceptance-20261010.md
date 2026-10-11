# Patched Tasker — interrupted manual-completion reconciliation

Date: 2026-10-10 (PDT)
Repository: user-maintained `Weintraub-B/tasker-core` fork; **not** an upstream Tasker contribution.
Feature branch: `fix/tasker-manual-recovery-acceptance-20261010`.

## What failed and why

Tasker's REST `complete_manually` writes the step transition/result, then independently transitions the parent task out of `blocked_by_failures`. A server crash or lost RPC between those commits can strand all downstream DAG steps, because `get_next_ready_tasks()` excludes blocked parents. Bridge correctly refuses blindly repeating an unknown-effect PATCH; this leaves recovery dependent on native Tasker reconciliation.

A second race was discovered in `TaskTransitionPersistence::transition_with_ownership`: its create path does not fence the persisted predecessor state under a lock. A stale recovery could theoretically compete with a cancellation even if the state-machine guard saw `blocked_by_failures` earlier.

## Changes in this fork

- `TaskTransitionPersistence::transition_if_current` locks the authoritative current transition row inside a PostgreSQL transaction and persists the named state-machine transition only when `to_state` still equals its exact expected predecessor state. If cancellation already committed, recovery is a no-op.
- `TaskStateMachine::transition` selects this fenced path **only** for `TaskEvent::ManualStepRecovered`. No unguarded SQL status update, speculative replay or new dispatch path is introduced.
- `BatchProcessor::reconcile_interrupted_manual_completions` runs before ordinary ready-task discovery, including when the latter would find no tasks. It considers at most the configured batch size, requires a current `blocked_by_failures` task transition and a most-recent `complete` workflow step transition whose recorded event is `CompleteManually`, and filters to `failed_steps = 0`. It then calls the existing `StepService` state-machine recovery helper, which rechecks live failure count and parent state. **Only Tasker's ordinary scheduler** enqueues dependents.
- A completed step with another failed predecessor still blocks the task. Terminal/cancelled tasks do not match the recovery query or fenced transition.
- Database tests explicitly cover cancellation-before-recovery and duplicate-exact-recovery behavior. A separately isolated Bridge/PostgreSQL canary exercises an interrupted step/parent write followed by a Tasker server restart.

## Acceptance and limits

- [x] Tasker/Rust source implementation added on a dedicated fork branch.
- [x] `cargo check --offline -p tasker-orchestration` with host-specific linker override, exit 0 (2026-10-10).
- [ ] Focused PostgreSQL database recovery/cancellation tests executed.
- [x] Rebuilt Tasker server compiled and isolated real PostgreSQL restart canary passed on 2026-10-10. The disposable canary emitted `ISOLATED_TASKER_POSTGRES_CANARY {ok:true,dependentExecutions:1,duplicateReleaseReplay:true}` and `ISOLATED_TASKER_CRASH_GAP_RECOVERY {ok:true,serverRestarted:true,initialParentState:"blocked_by_failures",finalStatus:"COMPLETED",originalExecutions:1,dependentExecutions:1}`; 1/1 opt-in integration test passed against a freshly built feature-branch server. The first runs exposed transient PostgreSQL Docker initialization and synthetic SQL quoting issues; those were harness defects, not accepted green gates.
- [ ] Multi-failed-predecessor, concurrent completion/cancellation and duplicate-delivery acceptance beyond unit fixtures.
- [ ] Reproducible pinned fork binary/SDK packaging, a bounded live *installed* service promotion, release rollback and production readback.
- [ ] Review of additional legacy Tasker transition paths for general CAS correctness. The present guard is intentionally limited to `ManualStepRecovered`.

**Never run the acceptance canary against resident PostgreSQL or the production Tasker server.** It creates a uniquely named disposable Docker container, uses randomized loopback ports, injects only the specific test state and tears the container down.

This document is a source and test-status record, **not** production-readiness certification.
