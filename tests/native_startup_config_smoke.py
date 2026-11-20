"""Read standard Worker input back through the installed native config CLI.

The actual startup projection/configuration fragments run with synthetic
credentials. No Matrix connection, model request or Task execution is performed.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

import yaml


def main():
    source = Path('/usr/local/bin/openhuman-worker-entrypoint.sh').read_text()
    projection = source[source.index('OPENCLAW_JSON='):source.index('\nif [ -n "${WORKER_NAME}" ]; then\n    # Native channel turns')]
    boot_start = source.index('if [ -n "${WORKER_NAME}" ]; then\n    # Native channel turns')
    bootstrap = source[boot_start:source.index('\npython3 /opt/agentteams/scripts/configure-teamharness.py', boot_start)]
    start = source.index('if [ -n "${BRIDGE_LLM_BASE_URL}" ] &&')
    model = source[start:source.index('\n# Preserve CLI context', start)]
    with tempfile.TemporaryDirectory() as temp:
        workspace = Path(temp)
        (workspace / 'skills').mkdir()
        (workspace / 'AGENTS.md').write_text('Complete your assigned task.')
        (workspace / 'SOUL.md').write_text('Be accurate.')
        runtime = workspace / 'runtime/runtime.yaml'
        runtime.parent.mkdir()
        runtime.write_text(yaml.safe_dump({
            'apiVersion': 'agentteams.io/v1beta1', 'kind': 'MemberRuntimeConfig',
            'member': {'runtime': 'openhuman', 'runtimeName': 'worker', 'matrixUserId': '@worker:test', 'personalRoomId': '!home:test'},
            'storage': {'memberPrefix': 'agents/worker'},
            'matrix': {'transport': {'homeserver': 'http://matrix.invalid', 'groupAllowFrom': ['@leader:test'],
                                   'dm': {'policy': 'allowlist', 'allowFrom': ['@human:test']}}},
            'desired': {'model': {'model': 'worker-standard-model', 'gatewayUrl': 'http://gateway.invalid'}},
            'credentials': {'matrixTokenEnv': 'SMOKE_MATRIX_BINDING', 'gatewayKeyEnv': 'SMOKE_MODEL_BINDING'},
        }))
        env = {**os.environ, 'WORKSPACE': temp, 'HOME': temp, 'OPENHUMAN_WORKSPACE': temp,
               'OPENHUMAN_CONFIG': str(workspace / 'config.toml'), 'AGENTTEAMS_WORKER_NAME': 'worker', 'WORKER_NAME': 'worker', 'WORKER_CR_NAME': 'worker', 'AGENTTEAMS_YOLO': '1',
               'SMOKE_MATRIX_BINDING': 'synthetic-matrix-token', 'SMOKE_MODEL_BINDING': 'synthetic-model-key'}
        subprocess.run(['bash', '-e', '-c', 'umask 077\nlog() { :; }\n' + projection + '\n' + bootstrap + '\n' + model],
                       env=env, check=True)
        assert not (workspace / 'openclaw.json').exists()
        assert not (workspace / 'agent-config').exists()
        # Check actual CLI readback, rather than assuming update exit status
        # means the intended configuration became effective.
        raw = subprocess.check_output(['openhuman-core', 'config', 'get'], env=env, text=True)
        native = json.loads(raw[raw.index('{'):])
        # The CLI response wraps the persisted configuration in config.
        cfg = native['result']['config']
        assert cfg['agent']['max_tool_iterations'] == 15
        assert cfg['autonomy']['max_actions_per_hour'] == 100
        assert {'wc', 'md5sum', 'sha256sum'} <= set(cfg['autonomy']['allowed_commands'])
        for command in cfg['autonomy']['allowed_commands']:
            subprocess.run(['sh', '-c', 'command -v "$1"', '_', command], check=True, stdout=subprocess.DEVNULL)
        assert cfg['default_model'] == 'worker-standard-model'
        assert cfg['inference_url'] == 'http://gateway.invalid/v1'
        assert cfg['api_key'] == 'synthetic-model-key'
        matrix = cfg['channels_config']['matrix']
        assert matrix['user_id'] == '@worker:test'
        assert matrix['room_id'] == '!home:test'
        assert matrix['access_token'] == 'synthetic-matrix-token'
        assert set(matrix['allowed_users']) == {'@leader:test', '@human:test'}
        assert (workspace / 'runtime/compat-config.json').stat().st_mode & 0o777 == 0o600
    print('PASS: standard Worker record -> startup fragments -> installed OpenHuman config CLI readback; no Matrix/model/Task execution')


if __name__ == '__main__':
    main()
