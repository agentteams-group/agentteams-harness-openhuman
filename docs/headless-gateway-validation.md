# Hosted local-openai authentication

The pinned SDK v0.57.18 predates upstream PR [#6249](https://github.com/tinyhumansai/openhuman/pull/6249). Its release factory required a TinyHumans session even for the independently authenticated `local-openai:` runtime. Unit builds skip that session gate, so earlier native fixtures alone did not establish a usable release startup path.

This backport permits only `local-openai:` with local runtime enabled, explicit opt-in and a nonempty caller-owned endpoint key to use independent authentication. Other provider families retain their existing session requirements. It exposes the existing `local_ai.api_key` field through the standard config update API; omission preserves the key and an empty value clears it. No app session is fabricated. Matrix identity, native Task binding, receipts, ApprovalGate, RunQueue and Tool Gate are unchanged.

Your Worker startup applies its configured platform Gateway through `LOCAL_OPENAI_URL`, sets runtime consent/key via `config update_local_ai_settings`, and explicitly selects `local-openai:<model>` for chat, reasoning, agentic, coding and memory workloads. It does not register the platform Gateway as a vendor-managed cloud service. Source/consent and credential scope still come from your Worker configuration.

Validation is separated:

- Python startup/contract regressions and replay of the entire patch against upstream v0.57.18 establish configuration/source consistency.
- SDK source regressions cover independent-auth eligibility and key set/preserve/clear. A binary-only build does not claim these full SDK unit suites were executed again.
- `tests/native_startup_config_smoke.py` reads the actual installed native configuration back.
- `tests/native_headless_gateway_smoke.py` must run against the newly built **release** binary in a network-disabled container. Its own authenticated loopback Gateway exercises native file write/read and `pwd`, rejects a wrong key, and verifies disabled runtime, missing consent/key and other provider families cannot use the exception. It uses no vendor session or real credentials. It is a native runtime fixture, not a real-provider collaborative task certification.

Standalone `local-openai` servers retain the SDK's prompt-guided tools. A Worker with both the Controller endpoint and its registered process identity uses native OpenAI function calls through the platform Gateway, matching its Tool Use Protocol. The release fixture runs both routes: it checks native function schemas, real file/write/read/shell results in subsequent model input, and an explicitly assigned Skill marker in the actual request. It runs as part of `make smoke`. No malformed tool text is executed as a substitute for a native call. The exact upstream SDK remains v0.57.18 (82d89ee853fb176a219d3c10373ec34e6619419e); these changes require a new native executable, not an old binary overlay.
