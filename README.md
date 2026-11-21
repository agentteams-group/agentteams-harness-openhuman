# AgentTeams OpenHuman Harness

The native runtime is pinned to OpenHuman **v0.57.18**. Direct-conversation
supplements use its native `RunQueue::Steer` at the next engine boundary;
Stop still cancels the captured execution and cleans up native tools. Late
supplements are retained as follow-up input. The Matrix approval bridge uses
the upgraded native `ApprovalGate`, with request-bound Dashboard cards and
sender/room/context checks.

The managed OpenHuman runtime integration: native Matrix patches, entrypoint,
Task-tool configuration, container packaging and compatibility tests. This code
was extracted without changing behavior from AgentTeams Core commit
`efa101387fdb1d7b7e3cd9199e6b1de4c2df517c`.

OpenHuman upstream source is pinned in `versions.env` and fetched from
[tinyhumansai/openhuman](https://github.com/tinyhumansai/openhuman). Core consumes
this repository as its `openhuman` Git submodule. Controller logic, shared Task
protocol, shared shell libraries and Agent role templates remain in
[AgentTeams Core](https://github.com/agentscope-ai/AgentTeams).

## Build and verify

Docker BuildKit named contexts keep this repository independently buildable.
`make build` supplies the pinned Core checkout as `agentteams`; local builds can
replace it with an explicit path:

```bash
make build VERSION=local-YYYYmmddHHMMSS \
  AT_CONTEXT=/path/to/AgentTeams \
  AGENTTEAMS_CONTROLLER_IMAGE=agentteams/agentteams-controller:local-YYYYmmddHHMMSS
make smoke VERSION=local-YYYYmmddHHMMSS
```

Build dependencies: Docker BuildKit; tests additionally require Rust, Python 3.11+
with pytest and PyYAML, and the Core source at `AT_SOURCE`:

```bash
make test AT_SOURCE=/path/to/AgentTeams
```

The image preserves native Matrix, task notification receipt validation, Task
MCP stdio tools, stop/supplement controls, approvals and session read receipts.
This migration does not add an HTTP notification service or enable new roles.
OpenHuman Manager end-to-end support is not validated here.

## Licensing

AgentTeams integration sources retain the Core Apache-2.0 license. The upstream
OpenHuman runtime and patches to it are subject to upstream GPL-3.0; its source
location and GPL license are preserved in the built image. See the Dockerfile
for the exact upstream ref and source build steps.
