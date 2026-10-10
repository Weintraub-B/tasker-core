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
- [x] Native Rust guard unit test: BlockedByFailures -> WaitingForDependencies on ManualStepRecovered, with terminal/other state rejection (executed on integrated source: 1 passed, 0 failed; 2026-10-10)
- [x] Build patched `tasker-server` in a separate isolated Tasker worktree
- [x] Agent Bridge `vitest.tasker-isolated.config.ts` passed (1 live acceptance test, ~14.66 s), with a fresh disposable PostgreSQL container, parent/child hold, independent replacement execution, guarded native release, exactly one downstream execution and GET-only duplicate reconciliation. Real Tasker proof: `ISOLATED_TASKER_POSTGRES_CANARY {ok:true,containerIsolated:true,originalFinalStatus:COMPLETED,nativeReleaseConfirmed:true,dependentExecutions:1,duplicateReleaseReplay:true}`. The server build included a test-only snapshot of **another worker's uncommitted immutable registration API files**; those files are NOT part of this Tasker fix commit, and upstream feature integration remains a separate prerequisite.
- [ ] Crash/restart and multi-failure task acceptance; concurrent manual completion and cancellation race audits
- [ ] Integrate a reviewed Tasker commit into canonical Tasker repository independently from Agent Bridge PR #81

**Safety:** The installed original Tasker service and database were not modified or restarted. The isolated PostgreSQL canary demonstrates the native DAG on a disposable server/worker, but does not prove the canonical Tasker version, production auth/permissions, multi-failure recovery or concurrent cancellation safety. The acceptance binary included uncommitted API snapshots from the other development worktree; do not present the source branch alone as independently runnable with Agent Bridge until those APIs are separately reviewed and integrated. Agent Bridge independently refuses to mark a dependency release verified while the parent still reports blocked.

## Integrated runner API + native recovery checkpoint (2026-10-10)

The historical test above used a disposable binary containing temporarily copied API files. **That prerequisite is now integrated in committed source** on local branch `integrate/agent-bridge-runner-manual-requeue-20261010`:

- `d118c512` — immutable template registration, task identity lookup, durable event replay, permissions, worker namespace selection, and FFI completion timeout support; changes preserved from the independent Tasker full-runner development branch.
- `4f9bec23` — merges manual-completion task recovery, state-machine guard, auditable event, and vendored `protoc` into that API branch, with combined `Cargo.lock`.
- The merged source's `tasker-shared`, `tasker-orchestration`, `tasker-worker`, and `tasker-ts` crates successfully built in the new integration worktree (`cargo build`, exit 0). The local compiler configuration needed temporary `cc` linker selection because this host's default `clang`/mold combination is unavailable; this override must not be committed.
- **PASS — new, independently built server:** explicit `vitest.tasker-isolated.config.ts` test against `BRIDGE_TASKER_CORE_ROOT=/data/projects/experiments/tasker-core-agent-bridge-integration-20261010` exited 0 (1 test passed; 38.49 s test duration, 39.21 s total). A fresh disposable PostgreSQL container, separate Tasker Rust server and TypeScript worker demonstrated a failed predecessor holding the dependent, a completed independent continuation, native guarded manual recovery, **exactly one** dependent execution, and GET-only duplicate reconciliation. Evidence: `ISOLATED_TASKER_POSTGRES_CANARY {ok:true,containerIsolated:true,originalFinalStatus:COMPLETED,nativeReleaseConfirmed:true,dependentExecutions:1,duplicateReleaseReplay:true}`.
- Agent Bridge at local feature head `78c6bf5` independently passed 1,270 standard tests (5 skipped, zero failed), typecheck, lint, and build. Its isolated real-PostgreSQL canary is not part of that default suite.

**Still blocked before merge/deployment:** Tasker upstream write authority / reviewed upstream integration; independent review of the combined API/security surface; broad Rust unit suite, multi-failure, cancellation races, restart/unknown-PATCH fault injection, and exact-head CI. The repair consists of a committed step transition followed by a separate parent-task transition: a crash between them may leave a completed step under a blocked task and requires a safe reconciliation strategy. Do not equate this successful isolated path with production readiness, and do not merge Agent Bridge draft PR #81 without its independent gates.

**Focused native guard result:** `cargo test test_operator_repaired_step_reenters_normal_dependency_discovery` completed with exit 0 (1 passed, 0 failed, 1,444 filtered out in `tasker-shared`; other integration test binaries had zero matching tests). The general Rust suite was intentionally not completed due to host resource contention; this focused result must not be reported as a full-suite pass.
