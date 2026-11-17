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

def test_existing_native_shared_data_requires_reconciliation_without_deletion(tmp_path):
    (tmp_path / 'runtime').mkdir()
    (tmp_path / 'runtime/runtime.yaml').write_text('member:\n  role: worker\n')
    shared = tmp_path / 'workspace/shared'
    shared.mkdir(parents=True)
    (shared / 'unverified-artifact.txt').write_text('do not overwrite authoritative artifacts')
    with pytest.raises(ValueError, match='Reconcile'):
        config.configure(tmp_path, agentteams / 'plugins/teamharness')
    assert (shared / 'unverified-artifact.txt').is_file()

def test_old_controller_without_member_contract_does_not_gain_a_broken_server(tmp_path, monkeypatch):
    monkeypatch.delenv('TEAMHARNESS_COORDINATION_URL', raising=False)
    config.configure(tmp_path, agentteams / 'plugins/teamharness')
    assert not (tmp_path / 'config.toml').exists()
    monkeypatch.setenv('TEAMHARNESS_COORDINATION_URL', 'http://controller/api/v1/coordination')
    with pytest.raises(ValueError):
        config.configure(tmp_path, agentteams / 'plugins/teamharness')
