"""Project the existing TeamHarness stdio server into OpenHuman's native MCP client."""
import json
import importlib.util
import os
from pathlib import Path
import shutil
import sys
import tempfile
import yaml


def configure(workspace: Path, bundle: Path):
    runtime = workspace / 'runtime/runtime.yaml'
    if not runtime.is_file():
        if os.getenv('AGENTTEAMS_CONTROLLER_URL'):
            raise ValueError('Task coordination requires your Controller-projected runtime.yaml')
        return
    native = workspace / 'workspace'
    shared = workspace / 'shared'
    shared.mkdir(parents=True, exist_ok=True)
    native_shared = native / 'shared'
    if native_shared.is_symlink():
        if native_shared.resolve() != shared.resolve():
            raise ValueError('Your native shared directory points outside your Worker workspace')
    else:
        if native_shared.exists():
            if not native_shared.is_dir():
                raise ValueError('Your native shared path must be a directory')
            if any(native_shared.iterdir()):
                # Keep old local files for inspection without importing stale Task data.
                backup = Path(tempfile.mkdtemp(prefix='openhuman-legacy-shared-', dir=runtime.parent))
                native_shared.rename(backup / 'shared')
                print(f'Preserved your previous native shared directory at {backup / "shared"}')
            else:
                native_shared.rmdir()
        native_shared.symlink_to(shared, target_is_directory=True)
    env = {'TEAMHARNESS_RUNTIME_CONFIG': str(runtime), 'TEAMHARNESS_SHARED_DIR': str(shared)}
    q = json.dumps
    block = '\n[mcp_client]\nenabled = true\n\n[[mcp_client.servers]]\nname = "teamharness"\ncommand = "python3"\n'
    block += 'args = [' + q(str(bundle / 'mcp/server.py')) + ']\n'
    block += 'env = {' + ', '.join(q(k) + ' = ' + q(v) for k, v in env.items()) + '}\n'
    with (workspace / 'config.toml').open('a') as target:
        target.write(block)
    record = yaml.safe_load(runtime.read_text()) or {}
    role = record.get('member', {}).get('role') or os.getenv('AGENTTEAMS_AGENT_ROLE', 'worker')
    native = workspace / 'workspace'
    role = {'team_leader': 'leader', 'standalone': 'worker'}.get(role, role)
    public_task = bundle / 'mcp/common/task_cli.py'
    spec = importlib.util.spec_from_file_location('openhuman_public_task_cli', public_task)
    task_cli = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(task_cli)
    schema = task_cli.schema(role)
    metadata = workspace / '.agentteams'
    metadata.mkdir(exist_ok=True)
    (metadata / 'native-task-schema.json').write_text(json.dumps(schema, ensure_ascii=False))
    manifest = yaml.safe_load((bundle / 'plugin.yaml').read_text())
    for skills in manifest['skills'].values():
        for skill in skills:
            if role in skill['roles']:
                shutil.copytree(bundle / skill['path'], native / 'skills' / skill['id'], dirs_exist_ok=True)
    with (native / 'SOUL.md').open('a') as prompt:
        prompt.write('\nUse your native task tool for Task operations. The task system prepares inputs, advances accepted dependencies and delivers results to the original conversation automatically. Preserve your assigned role; do not change Task metadata through shell uploads or send duplicate notifications. Use mcp_list_tools and mcp_call_tool only for your other configured MCP capabilities.\n')
        contract = (bundle / 'skills/team/task-management/SKILL.md').read_text()
        prompt.write('\n' + (contract.partition('## Plan as Leader')[0] if role == 'manager' else contract) + '\n')


if __name__ == '__main__':
    configure(Path(sys.argv[1]), Path(sys.argv[2]))
