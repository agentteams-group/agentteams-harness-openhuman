"""Execute the actual native-workspace bootstrap for each Controller role."""
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = (ROOT / 'scripts/openhuman-worker-entrypoint.sh').read_text()
BOOTSTRAP = re.search(r'if \[ .*?\n    # Native channel turns.*?\nfi\n(?=\npython3 /opt/agentteams/scripts/configure-teamharness.py)', SOURCE, re.S).group(0)
# Start at the conditional immediately before the native-channel comment.
BOOTSTRAP = BOOTSTRAP[BOOTSTRAP.rfind('\nif [', 0, BOOTSTRAP.index('# Native channel turns')) + 1:]

class NativeToolsTests(unittest.TestCase):
    def test_controller_roles_have_real_tools_and_their_own_identity(self):
        for role, yolo in [(role, yolo) for role in ['manager', 'standalone', 'team_leader', 'worker'] for yolo in ['0', '1']]:
            with self.subTest(role=role, yolo=yolo), tempfile.TemporaryDirectory() as directory:
                home = Path(directory)
                (home / 'skills').mkdir()
                (home / 'skills/task.md').write_text('Use your assigned task tools.')
                (home / 'AGENTS.md').write_text('You have your assigned Controller role.')
                (home / 'SOUL.md').write_text('Be accurate.')
                (home / 'config.toml').write_text('')
                result = subprocess.run(['bash', '-e', '-c', BOOTSTRAP], capture_output=True, text=True,
                    env={**os.environ, 'WORKSPACE': directory, 'WORKER_NAME': 'demo',
                         'AGENTTEAMS_WORKER_NAME': 'demo', 'WORKER_CR_NAME': 'demo',
                         'AGENTTEAMS_AGENT_ROLE': role, 'BRIDGE_ROOM_ID': '!room:test',
                         'AGENTTEAMS_STORAGE_PREFIX': 'agentteams/test-storage',
                         'BRIDGE_USER_ID': '@demo:test', 'AGENTTEAMS_YOLO': yolo})
                self.assertEqual(result.returncode, 0, result.stderr)
                definition = home / 'workspace/agents/orchestrator.toml'
                self.assertTrue(definition.exists(), 'native role definition is missing')
                agent = tomllib.loads(definition.read_text())
                self.assertTrue({'shell', 'file_read', 'file_write', 'list', 'glob', 'todo'} <= set(agent['tools']['named']))
                self.assertIn('member.role: ' + role, (home / 'workspace/SOUL.md').read_text())
                self.assertTrue((home / 'workspace/skills/task.md').exists())
                config = tomllib.loads((home / 'config.toml').read_text())
                self.assertEqual(config['agent']['max_tool_iterations'], agent['max_iterations'])
                policy = config['autonomy']
                if yolo == '1':
                    self.assertEqual(policy['max_actions_per_hour'], 100)
                else:
                    self.assertNotIn('max_actions_per_hour', policy)
                self.assertTrue({'mc', 'wc', 'md5sum', 'sha256sum'} <= set(policy['allowed_commands']))
                self.assertIn('agentteams/test-storage', (home / 'workspace/SOUL.md').read_text())
                self.assertIn('AgentTeams ' + role, agent['system_prompt']['inline'])

    def test_projected_team_role_overrides_stale_container_role(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            (home / 'skills').mkdir()
            (home / 'runtime').mkdir()
            (home / 'runtime/runtime.yaml').write_text('member:\n  role: team_leader\n')
            (home / 'AGENTS.md').write_text('Use your assigned role.')
            (home / 'SOUL.md').write_text('Be accurate.')
            (home / 'config.toml').write_text('')
            result = subprocess.run(['bash', '-e', '-c', BOOTSTRAP], capture_output=True, text=True,
                env={**os.environ, 'PATH': str(Path(sys.executable).parent)+os.pathsep+os.environ['PATH'],
                     'WORKSPACE': directory, 'WORKER_NAME': 'demo', 'WORKER_CR_NAME': 'demo',
                     'AGENTTEAMS_AGENT_ROLE': 'standalone', 'BRIDGE_ROOM_ID': '!room:test',
                     'BRIDGE_USER_ID': '@demo:test'})
            self.assertEqual(result.returncode, 0, result.stderr)
            prompt = (home / 'workspace/SOUL.md').read_text()
            self.assertIn('member.role: team_leader', prompt)
            definition = tomllib.loads((home / 'workspace/agents/orchestrator.toml').read_text())
            self.assertIn('AgentTeams team_leader', definition['system_prompt']['inline'])

if __name__ == '__main__':
    unittest.main()
