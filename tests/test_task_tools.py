import importlib.util
from pathlib import Path
import sys
import os
import tomllib

import pytest

root = Path(__file__).resolve().parents[1]
agentteams = Path(os.environ['AT_SOURCE']).resolve()
spec = importlib.util.spec_from_file_location('openhuman_task_tools', root / 'scripts/configure-teamharness.py')
config = importlib.util.module_from_spec(spec)
spec.loader.exec_module(config)

@pytest.mark.parametrize('role', ['manager', 'team_leader', 'worker', 'standalone'])
def test_native_stdio_mapping_uses_the_projected_identity_and_role_skills(tmp_path, monkeypatch, role):
    (tmp_path / 'runtime').mkdir()
    (tmp_path / 'runtime/runtime.yaml').write_text('member:\n  role: '+role+'\n')
    (tmp_path / 'config.toml').write_text('[channels_config.matrix]\nroom_id="!home:test"\n')
    (tmp_path / 'workspace').mkdir()
    (tmp_path / 'workspace/SOUL.md').write_text('Your existing role.\n')
    monkeypatch.setenv('AGENTTEAMS_AGENT_ROLE', role)
    config.configure(tmp_path, agentteams / 'plugins/teamharness')
    native = tomllib.loads((tmp_path / 'config.toml').read_text())
    server = native['mcp_client']['servers'][0]
    assert server['command'] == 'python3'
    assert server['name'] == 'teamharness'
    assert server['env']['TEAMHARNESS_SHARED_DIR'] == str(tmp_path / 'shared')
    assert Path(server['env']['TEAMHARNESS_RUNTIME_CONFIG']) == tmp_path / 'runtime/runtime.yaml'
    assert (tmp_path / 'workspace/shared').resolve() == tmp_path / 'shared'
    skills = {p.name for p in (tmp_path / 'workspace/skills').iterdir()}
    assert ('project-management' in skills) == (role == 'team_leader')
    assert ('task-execution' in skills) == (role in {'worker', 'standalone'})
    assert ('resource-management' in skills) == (role == 'manager')
    assert 'mcp_call_tool' in (tmp_path / 'workspace/SOUL.md').read_text()
    public_schema = __import__('json').loads((tmp_path / '.agentteams/native-task-schema.json').read_text())
    actions = public_schema['inputSchema']['properties']['action']['enum']
    assert ('create' in actions) == (role in {'manager', 'team_leader'})
    assert ('plan' in actions) == (role == 'team_leader')
    assert ('submit' in actions) == (role in {'worker', 'standalone'})
    prompt = (tmp_path / 'workspace/SOUL.md').read_text()
    assert 'Use your native task tool' in prompt
    assert 'When Task instructions name taskflow' not in prompt

def test_restarting_retains_worker_shared_artifacts_and_standard_role(tmp_path, monkeypatch):
    (tmp_path / 'runtime').mkdir()
    (tmp_path / 'runtime/runtime.yaml').write_text('member:\n  role: worker\n')
    (tmp_path / 'workspace').mkdir()
    monkeypatch.setenv('AGENTTEAMS_AGENT_ROLE', 'manager')
    for _ in range(2):
        (tmp_path / 'config.toml').write_text('[channels_config.matrix]\nroom_id="!home:test"\n')
        config.configure(tmp_path, agentteams / 'plugins/teamharness')
        assert not (tmp_path / 'workspace/skills/resource-management').exists()
        assert (tmp_path / 'workspace/skills/task-execution').is_dir()
        (tmp_path / 'workspace/shared/artifact.txt').write_text('persisted')
        assert (tmp_path / 'shared/artifact.txt').read_text() == 'persisted'

def test_switch_from_a_harness_with_legacy_shared_data_uses_the_worker_directory(tmp_path):
    (tmp_path / 'runtime').mkdir()
    (tmp_path / 'runtime/runtime.yaml').write_text('member:\n  role: worker\n')
    legacy = tmp_path / 'workspace/shared/tasks/task'
    legacy.mkdir(parents=True)
    (legacy / 'meta.json').write_text('old checkpoint')
    (legacy / 'unpublished.txt').write_text('old local artifact')
    shared = tmp_path / 'shared/tasks/task'
    shared.mkdir(parents=True)
    (shared / 'meta.json').write_text('current checkpoint')

    for _ in range(2):
        (tmp_path / 'config.toml').write_text('')
        config.configure(tmp_path, agentteams / 'plugins/teamharness')
        assert legacy.resolve() == shared
        assert (legacy / 'meta.json').read_text() == 'current checkpoint'
        assert not (shared / 'unpublished.txt').exists()

    backup, = (tmp_path / 'runtime').glob('openhuman-legacy-shared-*')
    assert (backup / 'shared/tasks/task/meta.json').read_text() == 'old checkpoint'
    assert (backup / 'shared/tasks/task/unpublished.txt').read_text() == 'old local artifact'


def test_native_shared_link_cannot_point_outside_the_worker_directory(tmp_path):
    (tmp_path / 'runtime').mkdir()
    (tmp_path / 'runtime/runtime.yaml').write_text('member:\n  role: worker\n')
    (tmp_path / 'workspace').mkdir()
    (tmp_path / 'workspace/shared').symlink_to(tmp_path / 'somewhere-else')
    with pytest.raises(ValueError, match='points outside'):
        config.configure(tmp_path, agentteams / 'plugins/teamharness')

def test_old_controller_without_member_contract_does_not_gain_a_broken_server(tmp_path, monkeypatch):
    monkeypatch.delenv('AGENTTEAMS_CONTROLLER_URL', raising=False)
    config.configure(tmp_path, agentteams / 'plugins/teamharness')
    assert not (tmp_path / 'config.toml').exists()
    monkeypatch.setenv('AGENTTEAMS_CONTROLLER_URL', 'http://controller')
    with pytest.raises(ValueError):
        config.configure(tmp_path, agentteams / 'plugins/teamharness')
