"""Test suite targeting boundary conditions, out-of-bounds stylus inputs, and invalid JSON files in CXX bridge / frontend.

Specifically, it checks the robustness of interactive input injection against edge cases and malformed formats.
"""

import json
import os
import subprocess
import sys
import tempfile
import pytest
from test_e2e import create_mock_rom

from emulator_harness import WORKSPACE, resolve_binary, spawn_interactive

EMULATOR_BIN = resolve_binary()


class TestCxxBoundaryConditions:
    """Boundary conditions and robustness tests for the CXX frontend extension."""

    @pytest.fixture(autouse=True)
    def setup_temp_dir(self) -> None:
        """Sets up a temporary directory for each test case."""
        # Detect workspace root relative to tests folder
        workspace_dir = str(WORKSPACE)
        local_temp = os.path.join(workspace_dir, "tests", "tmp")
        os.makedirs(local_temp, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=local_temp) as temp_dir:
            self.temp_dir = temp_dir
            yield

    def spawn_interactive(self) -> subprocess.Popen:
        """Spawns an interactive emulator process."""
        return spawn_interactive(EMULATOR_BIN, self.temp_dir)

    def test_out_of_bounds_stylus_coords_json(self) -> None:
        """Check behavior when out-of-bounds stylus inputs are injected via JSON."""
        proc = self.spawn_interactive()
        try:
            # 1. Extremely large coords (above u16 range)
            proc.stdin.write('INJECT {"nds_touch_x": 999999, "nds_touch_y": 888888, "nds_touch_pressed": true}\n')
            proc.stdin.flush()
            res = proc.stdout.readline().strip()
            assert "INJECT_OK" in res or "INJECT_ERROR" in res

            # 2. Negative stylus coordinates (should be parsed/cast or cleanly rejected)
            proc.stdin.write('INJECT {"nds_touch_x": -50, "nds_touch_y": -100, "nds_touch_pressed": true}\n')
            proc.stdin.flush()
            res = proc.stdout.readline().strip()
            assert "INJECT_OK" in res or "INJECT_ERROR" in res

            # 3. Float stylus coordinates (should extract base integer or reject)
            proc.stdin.write('INJECT {"nds_touch_x": 100.5, "nds_touch_y": 150.9, "nds_touch_pressed": true}\n')
            proc.stdin.flush()
            res = proc.stdout.readline().strip()
            assert "INJECT_OK" in res or "INJECT_ERROR" in res

            # Ensure process is still running and responsive
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_invalid_json_mismatched_braces(self) -> None:
        """Check parser behavior with malformed or unbalanced braces."""
        proc = self.spawn_interactive()
        try:
            # Unbalanced object braces
            proc.stdin.write('INJECT {"nds_touch_x": 100\n')
            proc.stdin.flush()
            res = proc.stdout.readline().strip()
            assert "INJECT_OK" in res or "INJECT_ERROR" in res

            # Unbalanced array braces
            proc.stdin.write('INJECT [{"frame": 0, "buttons": {"up": true}]\n')
            proc.stdin.flush()
            res = proc.stdout.readline().strip()
            assert "INJECT_OK" in res or "INJECT_ERROR" in res

            # Nesting braces inside string literals
            # This stresses custom tokenizer parsing since "}" is part of a comment string
            proc.stdin.write('INJECT [{"frame": 1, "comment": "mismatched } here", "buttons": {"up": true}}]\n')
            proc.stdin.flush()
            res = proc.stdout.readline().strip()
            assert "INJECT_OK" in res or "INJECT_ERROR" in res

            # Ensure responsiveness
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_json_mismatched_types(self) -> None:
        """Check behavior when JSON keys contain mismatched types (e.g. nested lists)."""
        proc = self.spawn_interactive()
        try:
            # Array inside button state value
            proc.stdin.write('INJECT {"nds_touch_x": [1, 2, 3], "nds_touch_y": {"val": 10}}\n')
            proc.stdin.flush()
            res = proc.stdout.readline().strip()
            assert "INJECT_OK" in res or "INJECT_ERROR" in res

            # Plain strings in booleans
            proc.stdin.write('INJECT {"up": "not_a_bool", "nds_touch_pressed": "yes"}\n')
            proc.stdin.flush()
            res = proc.stdout.readline().strip()
            assert "INJECT_OK" in res or "INJECT_ERROR" in res

            # Ensure responsiveness
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"
        finally:
            proc.terminate()
            proc.wait()
