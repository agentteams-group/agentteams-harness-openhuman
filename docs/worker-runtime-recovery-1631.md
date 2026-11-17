# Worker runtime configuration and recovery (#1631)

Status: partial implementation; Task/session continuation is not accepted yet.

OpenHuman now uses the stable Worker root from `AGENTTEAMS_WORKER_HOME`, or
`/root/agentteams-fs/agents/<worker>` when it is not supplied. Native-private
`OPENHUMAN_WORKSPACE` is not the configuration authority. Startup restores the
Worker prefix directly into this root and reads `runtime/runtime.yaml` in the
same location as the other Harnesses.

The Core standard projector checks format/version, selected Harness, Worker
identity and storage prefix, resolves declared credentials and writes the
private derived `runtime/compat-config.json`. A declared managed model does
not read an older compatibility object. Missing required bindings refuse
startup rather than retaining an old token/key. Older Controllers without a
standard record retain the external `openclaw.json` bridge; an undeclared
model still needs that compatibility model during migration. Native-only
settings are not yet fully represented by the standard record.

The existing TOML/native-model adapter consumes this projection. Model apply
failure stops startup. Matrix identity and personal room come from the Worker
record; a standard empty allowlist is not filled from stale environment policy.
The native Matrix adapter still combines DM/group sender lists as before;
separate per-room policy semantics need additional native validation.

TeamHarness reads `runtime/runtime.yaml` and the same Worker `shared/` directory
as storage sync. The native workspace links to this directory, including on
restart. Existing nonempty native shared directories are preserved and block
this initialization until their artifacts are reconciled, preventing an old
private copy from overwriting current shared artifacts.

Validation is deliberately separate:

- Host regressions execute startup path/projection fragments and MCP setup,
  including restart, shared artifacts and the Controller-projected role.
- `make smoke` runs standard projection/startup fragments against the actual
  installed native configuration CLI and checks effective model/Matrix values.
  It has no network, Matrix connection, model request or Task execution.
- Full source image/native controls use the pinned OpenHuman SDK and the Core
  SHA in `versions.env`; workflow checkouts must use that same Core SHA.

Still required: complete non-session settings/asset ownership, room receipt and
Task discovery at startup, process execution ownership, current-checkpoint
session reuse/rebuild, automatic inference without new Matrix input, saved
result delivery, unknown-effect verification and all switching/restart cases.
These changes alone do not complete agentscope-ai/AgentTeams#1631.
