"""Exercise release-mode auth and real native IO against an authenticated localhost Gateway.

No vendor session, Matrix service, model credential or production data is used.
Run with network disabled; the only provider is this process's loopback server.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

import yaml

KEY = "synthetic-caller-owned-key"
MARKER = "NATIVE_LOCAL_GATEWAY_IO_a17"
SKILL_MARKER = "ASSIGNED_NATIVE_SKILL_b28"


class Gateway(BaseHTTPRequestHandler):
    requests = []
    accepted = []
    native_tools = False

    def log_message(self, *_args):
        pass

    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        type(self).requests.append(self.headers.get("Authorization"))
        if self.headers.get("Authorization") != f"Bearer {KEY}":
            self.send_response(401)
            self.end_headers()
            self.wfile.write(b'{"error":{"message":"caller-owned authentication required"}}')
            return
        assert self.path == "/v1/chat/completions", self.path
        stage = len(type(self).accepted)
        type(self).accepted.append(payload)
        actions = [("file_write", {"path": "io.txt", "content": MARKER}),
                   ("file_read", {"path": "io.txt"}), ("shell", {"command": "pwd"})]
        if stage < len(actions):
            name, args = actions[stage]
            # Standalone local servers retain the SDK's prompt-guided mode.
            if self.native_tools:
                if stage == 0:
                    assert SKILL_MARKER in json.dumps(payload["messages"]), "assigned Skill missing from native model input"
                    print("PASS: assigned Skill is present in the actual model input", flush=True)
                assert any(t["function"]["name"] == name for t in payload.get("tools", [])), "native tool schema missing"
                content = None
                tool_calls = [{"id": f"call-{stage}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]
            else:
                content = "<tool_call>" + json.dumps({"name": name, "arguments": args}) + "</tool_call>"
        else:
            content = "NATIVE_LOCAL_GATEWAY_OK"
        message = {"role": "assistant", "content": content}
        if stage < len(actions) and self.native_tools:
            message["tool_calls"] = tool_calls
        data = json.dumps({"id": f"local-{stage}", "object": "chat.completion",
            "choices": [{"index": 0, "message": message, "finish_reason": "tool_calls" if message.get("tool_calls") else "stop"}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11}}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def run(env, *args, check=True):
    result = subprocess.run(["openhuman-core", *args], env=env, capture_output=True, text=True, timeout=90)
    if check:
        assert result.returncode == 0, (result.returncode, result.stdout[-2000:], result.stderr[-2000:])
    return result


def configure(root, endpoint, *, managed=False):
    private = root / "workspace"
    actions = private / "actions"
    private.mkdir(); actions.mkdir()
    runtime = root / "runtime/runtime.yaml"
    runtime.parent.mkdir()
    runtime.write_text(yaml.safe_dump({"apiVersion": "agentteams.io/v1beta1", "kind": "MemberRuntimeConfig",
        "member": {"runtime": "openhuman", "runtimeName": "worker", "matrixUserId": "@worker:test", "personalRoomId": "!home:test"},
        "storage": {"memberPrefix": "agents/worker"},
        "matrix": {"transport": {"homeserver": "http://matrix.invalid", "dm": {"policy": "open"}}},
        "desired": {"skills": ["fixture-skill"], "model": {"model": "fixture-model", "gatewayUrl": endpoint}},
        "credentials": {"matrixTokenEnv": "SMOKE_MATRIX_BINDING", "gatewayKeyEnv": "SMOKE_MODEL_BINDING"}}))
    env = {**os.environ, "WORKSPACE": str(root), "HOME": str(root), "OPENHUMAN_WORKSPACE": str(root),
        "OPENHUMAN_ACTION_DIR": str(actions), "OPENHUMAN_CONFIG": str(root / "config.toml"),
        "AGENTTEAMS_WORKER_NAME": "worker", "AGENTTEAMS_WORKER_HOME": str(root), "WORKER_NAME": "worker", "WORKER_CR_NAME": "worker", "SMOKE_MATRIX_BINDING": "synthetic-matrix", "SMOKE_MODEL_BINDING": KEY,
        "LOCAL_OPENAI_URL": endpoint + "/v1", "RUST_LOG": "error", "AGENTTEAMS_OTEL_ENABLED": "false"}
    for key in ("AGENTTEAMS_CONTROLLER_URL", "AGENTTEAMS_AUTH_TOKEN", "AGENTTEAMS_AUTH_TOKEN_FILE"):
        env.pop(key, None)
    if managed:
        env.update(AGENTTEAMS_CONTROLLER_URL="http://controller.invalid", AGENTTEAMS_PROCESS_ID="fixture-process", AGENTTEAMS_AGENT_ROLE="worker")
    skill = root / "skills/fixture-skill"
    skill.mkdir(parents=True)
    (skill / "SKILL.md").write_text("You must preserve the marker " + SKILL_MARKER + " in your assigned task context.")
    (root / "AGENTS.md").write_text("You execute the requested task using your native tools.")
    (root / "SOUL.md").write_text("Be accurate.")
    source = Path("/usr/local/bin/openhuman-worker-entrypoint.sh").read_text()
    projection = source[source.index("OPENCLAW_JSON="):source.index('\npython3 /opt/agentteams/scripts/configure-teamharness.py')]
    start = source.index('if [ -n "${BRIDGE_LLM_BASE_URL}" ] &&')
    model = source[start:source.index("\n# Preserve CLI context", start)]
    result = subprocess.run(["bash", "-e", "-c", "umask 077\nlog() { :; }\n" + projection + "\n" + model],
        env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, (result.stdout[-1000:], result.stderr[-1000:])
    if managed:
        subprocess.run(["python3", "/opt/agentteams/scripts/configure-teamharness.py", str(root),
                        "/opt/agentteams/plugins/teamharness"], env=env, check=True, capture_output=True, text=True, timeout=30)
    run(env, "config", "update_autonomy_settings", "--level", "supervised",
        "--allowed_commands", '["pwd"]', "--auto_approve", '["file_write","file_read","shell"]')
    return env, private, actions


def chat(env):
    return run(env, "call", "--method", "openhuman.agent_chat", "--params",
               json.dumps({"message": "Create io.txt, read it, then run pwd."}), check=False)


def main():
    gateway = ThreadingHTTPServer(("127.0.0.1", 0), Gateway)
    threading.Thread(target=gateway.serve_forever, daemon=True).start()
    endpoint = f"http://127.0.0.1:{gateway.server_port}"
    try:
        with tempfile.TemporaryDirectory() as temp:
            env, private, actions = configure(Path(temp), endpoint)
            result = chat(env)
            assert result.returncode == 0, (result.stdout[-2500:], result.stderr[-2500:], len(Gateway.requests))
            assert (actions / "io.txt").read_text() == MARKER
            assert not (private / "io.txt").exists()
            assert len(Gateway.accepted) == 4, len(Gateway.accepted)
            assert "NATIVE_LOCAL_GATEWAY_OK" in result.stdout
            last = json.dumps(Gateway.accepted[-1])
            assert MARKER in last and str(actions) in last, "real file/read/pwd results missing from next model input"
            assert not list(Path(temp).rglob("app-session*")), "fixture must not create vendor sessions"
            # The same persisted configuration API sets, preserves and clears auth.
            run(env, "config", "update_local_ai_settings", "--runtime_enabled", "true")
            cfg = run(env, "config", "get").stdout
            assert KEY in cfg
            for name, args in [
                ("runtime-disabled", ["--runtime_enabled", "false"]),
                ("consent-disabled", ["--runtime_enabled", "true", "--opt_in_confirmed", "false"]),
                ("own-key-cleared", ["--opt_in_confirmed", "true", "--api_key", " "]),
            ]:
                run(env, "config", "update_local_ai_settings", *args)
                before = len(Gateway.requests)
                failure = chat(env)
                assert failure.returncode != 0 and len(Gateway.requests) == before, name
                assert "SESSION_EXPIRED" in failure.stdout + failure.stderr, (name, failure.stdout[-800:], failure.stderr[-800:])
            run(env, "config", "update_local_ai_settings", "--api_key", KEY)
            for provider in ("openai:fixture-model", "ollama:fixture-model"):
                run(env, "config", "update_model_settings", "--chat_provider", provider, "--reasoning_provider", provider)
                before = len(Gateway.requests)
                failure = chat(env)
                assert failure.returncode != 0 and len(Gateway.requests) == before, provider
                assert "SESSION_EXPIRED" in failure.stdout + failure.stderr, provider
            run(env, "config", "update_model_settings", "--chat_provider", "local-openai:fixture-model", "--reasoning_provider", "local-openai:fixture-model")
            run(env, "config", "update_local_ai_settings", "--api_key", "synthetic-wrong-key")
            before = len(Gateway.requests)
            failure = chat(env)
            assert failure.returncode != 0 and len(Gateway.requests) > before
            assert len(Gateway.accepted) == 4, "wrong own-endpoint key must not execute tools"
            assert "401" in failure.stdout + failure.stderr
        Gateway.accepted.clear()
        Gateway.native_tools = True
        with tempfile.TemporaryDirectory() as temp:
            env, private, actions = configure(Path(temp), endpoint, managed=True)
            result = chat(env)
            assert result.returncode == 0, (result.stdout[-2500:], result.stderr[-2500:])
            assert (actions / "io.txt").read_text() == MARKER
            assert len(Gateway.accepted) == 4
            first = Gateway.accepted[0]
            assert SKILL_MARKER in json.dumps(first["messages"]), "assigned Skill never reached the native model request"
            last = Gateway.accepted[-1]["messages"]
            assert MARKER in json.dumps(last)
            assert any(m.get("role") == "tool" for m in last), "native tool results were not preserved"
        print("PASS: actual managed native function schemas/calls/results and assigned Skill reached the model; standalone local XML unchanged")
        print("PASS: actual release CLI startup -> authenticated local-openai Gateway -> native file_write/read/pwd; runtime/consent/key gates and cloud sessions preserved; wrong own-key rejected; no vendor session or real model")
    finally:
        gateway.shutdown()


if __name__ == "__main__":
    main()
