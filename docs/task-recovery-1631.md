# Worker Task recovery candidate

This candidate uses Core's `task_recovery_rpc.py` boundary for discovery,
conditional claim, current execution authority, result persistence and delivery
receipts. It does not introduce a second Task HTTP implementation or Project
list permission. `AGENTTEAMS_PROCESS_ID` comes from the startup supervisor.

The native Matrix listener starts recovery before admitting ordinary messages.
A verified assignment notification wakes discovery instead of starting a second
ungated turn. Discovery can enqueue an existing Task without a new Matrix event;
internal recovery inputs are not represented as fabricated Matrix event IDs.
Before discovery, the Matrix listener registers the actual Worker start. A saved
result uses the current SDK account and device from `whoami`. Core binds the
first sending device before the SDK sends the encrypted or plaintext event.
After a device change, the SDK reads room history and verifies the original
sender, transaction marker and result bytes before acknowledging delivery.

The native dispatcher restores its actual `ChatMessage` history only when its
Worker authority, team/task scope, runtime, expiry, checkpoint digest and native
history digest match. Missing or stale history creates a new native thread with
the current shared recovery context. The tool loop persists its actual tool
results at iteration boundaries; it never writes authoritative checkpoints from
old session data. Cache files are atomically published with private permissions.

Execution authority is checked before inference and each tool, and watched while
the turn runs. Dropping a cancelled native MCP request now drops its owned stdio
session and kills the server process group. Shell Task tools receive the current
process identity and credential-file path, without forwarding raw model keys or
loading an old process identity. Interrupted native effects are fenced through
the shared unknown-outcome protocol. Startup recovery parks unresolved effects
until shared verification evidence is saved; it does not start a replay turn.
A released execution cannot admit another tool call. This requires verification
before another effect; cancellation cannot prove an external service rolled back.

## Validation boundary

- Shared Core dependency: `d49cc3a9af3dcbc38463f34b0e1b881dcfe56f55`. The native Task cache now checks the shared session descriptor and publishes its digest after writing the local native record. Earlier test proofs below retain their actual older Core SHA.
- Upstream: `v0.54.0`, `c25fc8e5fd3e7716f416571cd53b34247c94ddbe`.
- All six packaged patches reapplied to that upstream; integrated files matched
  the edited source checkout.
- Existing Harness host suite: 12 tests and 3 subtests passed; the actual receipt
  Rust module's test passed; entrypoint/wrapper shell syntax passed.
- New native tests cover scoped/current/unexpired tool-history reuse and stale,
  foreign, corrupted or missing caches, plus cancellation of a real stdio MCP
  server and its child followed by session reinitialization. These tests are
  packaged in `task-recovery.patch`. Hosted native compilation and all five
  Rust tests passed at `b70b490` and at `d6a4fd3` (including the startup verification
  fence and released-tool check). The former's independent image/smoke CI also
  passed. See `tests/evidence/worker-task-native-controls-1631.json` for exact SHAs,
  job URLs, log hashes and limits; the latter's image job is still running.

This is still a Draft implementation. Real model/Controller/Matrix Task continuation, reply-only recovery, authority conflicts, unknown
effects, cancellation/reassignment and the full restart/switch matrix remain
unaccepted. A patch-application check and the existing receipt test do not prove
Task execution recovery. The common persistent handoff state machine and shared
Dashboard/CLI switching flow also remain open in Core issue #1631.
