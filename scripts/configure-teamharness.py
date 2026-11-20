"""Project the existing TeamHarness stdio server into OpenHuman's native MCP client."""
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import yaml


def configure(workspace: Path, bundle: Path):
    runtime = workspace / 'runtime/runtime.yaml'
    if not runtime.is_file():
        if os.getenv('TEAMHARNESS_COORDINATION_URL'):
            raise ValueError('Task coordination requires your Controller-projected runtime.yaml')
        return
    native = workspace / 'workspace'
    native.mkdir(parents=True, exist_ok=True)
    shared = workspace / 'shared'
    native_shared = native / 'shared'
    if shared.is_symlink():
        if native_shared.is_symlink() or shared.resolve() != native_shared.resolve():
            raise ValueError('Your shared directory points outside your native workspace')
    else:
        shared.mkdir(parents=True, exist_ok=True)
        if native_shared.is_symlink():
            if native_shared.resolve() != shared.resolve():
                raise ValueError('Your native shared directory points outside your Worker workspace')
            native_shared.unlink()
        elif native_shared.exists():
            if not native_shared.is_dir():
                raise ValueError('Your native shared path must be a directory')
            if any(native_shared.iterdir()):
                backup = Path(tempfile.mkdtemp(prefix='openhuman-legacy-shared-', dir=runtime.parent))
                native_shared.rename(backup / 'shared')
                print(f'Preserved your previous native shared directory at {backup / "shared"}')
            else:
                native_shared.rmdir()
        shared.rename(native_shared)
        shared.symlink_to(native_shared, target_is_directory=True)
    env = {'TEAMHARNESS_RUNTIME_CONFIG': str(runtime), 'TEAMHARNESS_SHARED_DIR': str(native_shared)}
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
    manifest = yaml.safe_load((bundle / 'plugin.yaml').read_text())
    for skills in manifest['skills'].values():
        for skill in skills:
            if role in skill['roles']:
                shutil.copytree(bundle / skill['path'], native / 'skills' / skill['id'], dirs_exist_ok=True)
    with (native / 'SOUL.md').open('a') as prompt:
        prompt.write('\nYou have the native MCP bridge tools. When Task instructions name taskflow, projectflow, filesync, artifact, roomflow or message, call mcp_call_tool with server "teamharness", tool set to that name, and arguments set to its JSON arguments. Use mcp_list_tools on server "teamharness" for schemas. Preserve your assigned role; do not change Task metadata through shell uploads.\n')


if __name__ == '__main__':
    configure(Path(sys.argv[1]), Path(sys.argv[2]))
