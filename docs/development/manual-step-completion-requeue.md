# Manual workflow-step completion must reopen task scheduling

**Integration regression:** Agent Bridge's isolated PostgreSQL two-step DAG canary (2026-10-10).  
**Scope:** Tasker REST `complete_manually` repair of a permanently failed predecessor, **not** automatic job retry, blanket DAG unblocking, or privileged agent execution.

## Reproduced failure

The original Tasker's PATCH `/v1/tasks/{task_uuid}/workflow_steps/{step_uuid}` with `action_type: complete_manually` saved the corrected step result and transitioned the predecessor step to `complete`, but never moved the parent task from `blocked_by_failures`. PostgreSQL's `get_task_execution_context` returned `has_ready_steps`, and the dependent step remained `pending`, but the Tasker step enqueuer does not process a parent task that is still blocked.

The code path `StepService::resolve_step_manually` invoked only `StepStateMachine::transition`. Ordinary worker results pass through `OrchestrationResultProcessor`/task finalization and re-enqueue logic; the manual API skipped that coordinator.

## Fix in isolated Tasker source branch

`StepService::reopen_after_manual_step_completion` is called only **after** a `CompleteManually` step transition completes. It uses the current database execution context to verify `failed_steps == 0`. Only a parent task actually in `BlockedByFailures` may transition via a named, audited `TaskEvent::ManualStepRecovered(step_uuid)` to `WaitingForDependencies`, and the standard Tasker step enqueuer decides what is ready next. If other failures persist, do not unblock. If the parent has progressed, been cancelled, or entered a terminal state, do not reset it.

No direct enqueue is performed from the REST endpoint and no direct PostgreSQL state UPDATE shortcuts the state-machine guard. Tasker transition audit names the repaired step.

The `tasker-shared` gRPC build now uses pinned `protoc-bin-vendored` if `PROTOC` is unset, allowing consistent compilation in isolated, confined environments.

## Acceptance requirements

- [x] Cargo check of `tasker-shared` and `tasker-orchestration` succeeded, using the isolated compiler configuration
- [ ] Native Rust unit test: BlockedByFailures -> WaitingForDependencies on ManualStepRecovered, terminal/other states reject it (written, not executed; Rust unit-test tooling is still pending)
- [x] Build patched `tasker-server` in a separate isolated Tasker worktree
- [x] Agent Bridge `vitest.tasker-isolated.config.ts` passed (1 live acceptance test, ~14.66 s), with a fresh disposable PostgreSQL container, parent/child hold, independent replacement execution, guarded native release, exactly one downstream execution and GET-only duplicate reconciliation. Real Tasker proof: `ISOLATED_TASKER_POSTGRES_CANARY {ok:true,containerIsolated:true,originalFinalStatus:COMPLETED,nativeReleaseConfirmed:true,dependentExecutions:1,duplicateReleaseReplay:true}`. The server build included a test-only snapshot of **another worker's uncommitted immutable registration API files**; those files are NOT part of this Tasker fix commit, and upstream feature integration remains a separate prerequisite.
- [ ] Crash/restart and multi-failure task acceptance; concurrent manual completion and cancellation race audits
- [ ] Integrate a reviewed Tasker commit into canonical Tasker repository independently from Agent Bridge PR #81

**Safety:** The installed original Tasker service and database were not modified or restarted. The isolated PostgreSQL canary demonstrates the native DAG on a disposable server/worker, but does not prove the canonical Tasker version, production auth/permissions, multi-failure recovery or concurrent cancellation safety. The acceptance binary included uncommitted API snapshots from the other development worktree; do not present the source branch alone as independently runnable with Agent Bridge until those APIs are separately reviewed and integrated. Agent Bridge independently refuses to mark a dependency release verified while the parent still reports blocked.
