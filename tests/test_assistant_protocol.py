"""Compile and execute the actual Rust carrier against the shared Controller fixture."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

import pytest

core = Path(os.environ["AT_SOURCE"])
sys.path.insert(0, str(core / "tests/lib"))
from assistant_fixture import AssistantFixture


@pytest.mark.parametrize("replaced", [False, True])
def test_native_rust_assistant_uses_public_receipt_and_role_context(tmp_path, replaced):
    cargo = shutil.which("cargo")
    if not cargo:
        pytest.skip("Rust toolchain required for native admission certification")
    with AssistantFixture() as f:
        if replaced:
            f.context["owner"] = "replaced"
        wrapper = tmp_path / "python3"
        wrapper.write_text('#!/bin/sh\nshift\nexec "$AT_NATIVE_TEST_PYTHON" "$AT_SOURCE/shared/lib/task_coordination.py" "$@"\n')
        wrapper.chmod(0o700)
        env = {**os.environ, "PATH": str(tmp_path) + os.pathsep + os.environ["PATH"],
            "AT_NATIVE_TEST_PYTHON": sys.executable, "AT_ASSISTANT_TEST_CONTENT": json.dumps(f.content())}
        if replaced:
            env["AT_ASSISTANT_EXPECT_REJECT"] = "1"
        subprocess.run([cargo, "test", "--manifest-path", str(Path(__file__).parent / "assistant-protocol/Cargo.toml"),
            "assistant_uses_the_same_verified_native_teams_boundary", "--", "--nocapture"], env=env, check=True)
