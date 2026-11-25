"""Execute the startup path/projection fragments, without a Matrix/native daemon."""
import json
import os
from pathlib import Path
import subprocess

import pytest
import yaml

source = (Path(__file__).parents[1] / 'scripts/openhuman-worker-entrypoint.sh').read_text()


def test_member_prompt_distinguishes_ordinary_workspace_from_prepared_task_output(tmp_path):
    (tmp_path / 'AGENTS.md').write_text('你是测试成员。')
    (tmp_path / 'SOUL.md').write_text('你使用中文沟通。')
    start = source.index('    write_member_prompt() {')
    end = source.index('\n    # Channel turns load', start)
    result = subprocess.run(['bash', '-e', '-c', source[start:end] + '\nwrite_member_prompt'],
        text=True, capture_output=True, env={**os.environ, 'WORKSPACE': str(tmp_path),
        'NATIVE_WORKSPACE': str(tmp_path / 'workspace'), 'WORKER_CR_NAME': 'tester',
        'NATIVE_ROLE': 'worker', 'BRIDGE_ROOM_ID': '!room:test', 'BRIDGE_USER_ID': '@tester:test'})
    assert result.returncode == 0, result.stderr
    assert '普通会话' in result.stdout and str(tmp_path / 'workspace') in result.stdout
    assert 'Task 中' in result.stdout and 'outputDirectory' in result.stdout
    assert '不要把 shared/tasks/' in result.stdout
    assert 'Create relative files there' not in result.stdout


def test_member_prompt_loads_only_assigned_skill_bodies_and_refreshes(tmp_path):
    (tmp_path / 'AGENTS.md').write_text('你是测试成员。')
    (tmp_path / 'SOUL.md').write_text('你使用中文沟通。')
    (tmp_path / 'runtime').mkdir()
    (tmp_path / 'runtime/runtime.yaml').write_text(yaml.safe_dump({'desired': {'skills': ['tester', 'escape']}}))
    for name, body in [('tester', '核对准确 head，使用真实浏览器验收。'), ('unassigned', '不应加载的其他小队规则。')]:
        directory = tmp_path / 'skills' / name
        directory.mkdir(parents=True)
        (directory / 'SKILL.md').write_text(body)
    outside = tmp_path / 'private'
    outside.mkdir()
    (outside / 'SKILL.md').write_text('不应读取的目录外内容。')
    (tmp_path / 'skills/escape').symlink_to(outside, target_is_directory=True)
    start = source.index('    write_member_prompt() {')
    end = source.index('\n    # Channel turns load', start)
    for body in ['核对准确 head，使用真实浏览器验收。', '新版规则：先克隆已接受的 bundle。']:
        (tmp_path / 'skills/tester/SKILL.md').write_text(body)
        result = subprocess.run(['bash', '-e', '-c', source[start:end] + '\nwrite_member_prompt'],
            text=True, capture_output=True, env={**os.environ, 'WORKSPACE': str(tmp_path),
            'NATIVE_WORKSPACE': str(tmp_path / 'workspace'), 'WORKER_CR_NAME': 'tester',
            'NATIVE_ROLE': 'worker', 'BRIDGE_ROOM_ID': '!room:test', 'BRIDGE_USER_ID': '@tester:test'})
        assert result.returncode == 0, result.stderr
        assert result.stdout.count(body) == 1
        assert '不应加载' not in result.stdout
        assert '不应读取' not in result.stdout


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


def test_native_session_history_is_synced_without_uploading_projected_runtime_or_credentials(tmp_path):
    workspace, remote, binary = (tmp_path / name for name in ['worker', 'remote', 'bin'])
    binary.mkdir()
    for kind in ['task', 'message']:
        directory = workspace / f'runtime/openhuman-{kind}-sessions'
        directory.mkdir(parents=True)
        (directory / 'own.json').write_text(json.dumps({'history': ['real tool result', 'actual sent fact']}))
    (workspace / 'runtime/runtime.yaml').write_text('controller-owned projection')
    (workspace / 'credentials').mkdir()
    (workspace / 'credentials/token').write_text('synthetic-private-token')
    marker = workspace / '.last-pull'
    marker.touch()
    mc = binary / 'mc'
    mc.write_text('#!/usr/bin/env python3\nimport shutil,sys\nassert sys.argv[1]=="mirror"\n'
                  'shutil.copytree(sys.argv[2],sys.argv[3],dirs_exist_ok=True)\n')
    mc.chmod(0o700)
    start = source.index('(\n    while true; do', source.index('# Local → Remote'))
    end = source.index('\n) &', start) + len('\n)')
    script = 'count=0\nsleep(){ count=$((count+1)); if [ "$count" -gt 1 ]; then exit 0; fi; }\n'
    script += 'ensure_mc_credentials(){ :; }\nlog(){ :; }\nTASK_SYNC_EXCLUDES=()\n' + source[start:end]
    result = subprocess.run(['bash', '-e', '-c', script], capture_output=True, text=True,
        env={**os.environ, 'PATH': str(binary) + ':' + os.environ['PATH'], 'WORKSPACE': str(workspace),
             'PULL_MARKER': str(marker), 'AGENTTEAMS_STORAGE_PREFIX': str(remote), 'WORKER_NAME': 'worker'})
    assert result.returncode == 0, result.stderr
    for kind in ['task', 'message']:
        target = remote / f'agents/worker/runtime/openhuman-{kind}-sessions/own.json'
        assert json.loads(target.read_text())['history'] == ['real tool result', 'actual sent fact']
    assert not (remote / 'agents/worker/runtime/runtime.yaml').exists()
    assert not (remote / 'agents/worker/credentials/token').exists()


def test_generated_native_turn_budget_leaves_work_time_after_human_approval(tmp_path):
    import tomllib
    start = source.index('cat > "${WORKSPACE}/config.toml" <<EOF')
    end = source.index('\nEOF', start) + len('\nEOF')
    result = subprocess.run(['bash', '-e', '-c', source[start:end]], capture_output=True, text=True,
        env={**os.environ, 'WORKSPACE': str(tmp_path), 'BRIDGE_HOMESERVER': 'http://matrix.test',
             'BRIDGE_ACCESS_TOKEN': 'fixture', 'BRIDGE_ROOM_ID': '!room:test', 'BRIDGE_USER_ID': '',
             'ALLOWED_USERS_TOML': '"@human:test"', 'MATRIX_DEVICE_ID': ''})
    assert result.returncode == 0, result.stderr
    config = tomllib.loads((tmp_path / 'config.toml').read_text())
    assert config['channels_config']['message_timeout_secs'] >= 300 + 600
    assert config['channels_config']['matrix']['room_id'] == '!room:test'
