"""Execute the startup path/projection fragments, without a Matrix/native daemon."""
import json
import os
from pathlib import Path
import subprocess

import pytest
import yaml

source = (Path(__file__).parents[1] / 'scripts/openhuman-worker-entrypoint.sh').read_text()


@pytest.mark.parametrize('role', ['worker', 'manager', 'team_leader'])
def test_member_home_and_native_file_action_directory_are_distinct(tmp_path, role):
    start = source.index('WORKER_NAME=')
    end = source.index('\nlog()', start)
    result = subprocess.run(['bash', '-e', '-c', source[start:end] + '\nprintf "%s\\n" "$WORKSPACE" "$HOME" "$OPENHUMAN_WORKSPACE" "$OPENHUMAN_ACTION_DIR"'],
                            text=True, capture_output=True, env={**os.environ,
                            'AGENTTEAMS_WORKER_NAME': 'worker', 'AGENTTEAMS_AGENT_ROLE': role,
                            'AGENTTEAMS_WORKER_HOME': str(tmp_path), 'OPENHUMAN_WORKSPACE': '/old/private/native-home',
                            'OPENHUMAN_ACTION_DIR': '/old/project-directory'})
    assert result.returncode == 0, result.stderr
    assert result.stdout.splitlines() == [str(tmp_path)] * 3 + [str(tmp_path / 'workspace')]


def test_standard_projection_works_without_compatibility_or_private_directory(tmp_path):
    record = {
        'apiVersion': 'agentteams.io/v1beta1', 'kind': 'MemberRuntimeConfig',
        'member': {'runtime': 'openhuman', 'runtimeName': 'worker', 'matrixUserId': '@worker:test', 'personalRoomId': '!home:test'},
        'storage': {'memberPrefix': 'agents/worker'},
        'matrix': {'transport': {'homeserver': 'https://matrix.test', 'groupAllowFrom': ['@leader:test']}},
        'desired': {'model': {'model': 'current-model', 'gatewayUrl': 'https://gateway.test'}},
        'credentials': {'matrixTokenEnv': 'BOUND_MATRIX_TOKEN', 'gatewayKeyEnv': 'BOUND_GATEWAY_KEY'},
    }
    (tmp_path / 'runtime').mkdir()
    (tmp_path / 'runtime/runtime.yaml').write_text(yaml.safe_dump(record))
    start = source.index('OPENCLAW_JSON=')
    end = source.index('\nif [ -f "${OPENCLAW_JSON}" ]', start)
    result = subprocess.run(['bash', '-e', '-c', source[start:end] + '\nprintf "%s" "$OPENCLAW_JSON"'],
                            capture_output=True, text=True, env={**os.environ,
                            'WORKSPACE': str(tmp_path), 'AGENTTEAMS_WORKER_NAME': 'worker',
                            'AGENTTEAMS_LOG_LIB_DIR': str(Path(os.environ['AT_SOURCE']) / 'shared/lib'),
                            'BOUND_MATRIX_TOKEN': 'synthetic-matrix-token', 'BOUND_GATEWAY_KEY': 'synthetic-key'})
    assert result.returncode == 0, result.stderr
    projected = Path(result.stdout)
    assert projected == tmp_path / 'runtime/compat-config.json'
    cfg = json.loads(projected.read_text())
    assert cfg['channels']['matrix']['userId'] == '@worker:test'
    assert cfg['channels']['matrix']['homeRoomId'] == '!home:test'
    assert cfg['models']['providers']['agentteams-gateway']['apiKey'] == 'synthetic-key'
    assert not (tmp_path / 'openclaw.json').exists()
    assert not (tmp_path / '.openhuman').exists()
