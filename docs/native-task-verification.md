# OpenHuman native Task binding verification

The runtime retains main's OpenHuman v0.57.18 RunQueue steering, ApprovalGate, shell audit and standard startup. Task business rules remain in the shared Core bridge pinned to 663615d7e5d5a85887999b75b510347a1edfdbcc. It registers the fixed native Task factory, binds source/phase/receipts to each turn, and restricts native file/command IO to the authoritative Task directories without changing memory/session roots or process cwd.

The unified engine performs input checkpoints and canonical admission before sampling and each tool, keeps genuine unknown outcomes fenced, and ends a completed phase using the public receipt. Native model/tool tracing exports timing, usage and trusted identity without content. File validation uses the new SDK's supported per-call action directory and trusted roots; budget checks and increments always use the original policy tracker, because cloning a v0.57 tracker copies its counter.

Tests remain separate proof layers:

- Python startup/role/schema tests against the exact shared Core.
- `tests/task-protocol`: native binding source with explicit SDK interface stubs and real common bridge/exporter; not a full SDK certification.
- Patched SDK `agentteams_native_task_protocol_` unit fixture: real unified engine, actual public Task module with synthetic API responses, actual RunQueue, native file/shell policies and original action budget, terminal receipt, and a real OTLP receiver with content-exclusion checks.
- Main native steering, Matrix control/approval and tool cancellation fixtures are retained and run on the combined source.

The v0.54 binary and prior image cannot represent this upgrade. A new v0.57 build must pass before image readiness. Native fixtures use synthetic input/provider/API, and never count as real-task certification. Real requirements-only simple/complex scenarios remain a later coordinated step.
