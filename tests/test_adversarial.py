"""Tier 5 Adversarial Test Suite for the FFI Bridge Emulator.

This suite targets edge cases, input validation gaps, integer overflows,
and denial-of-service conditions in the compiled C++ emulator binary.
"""

import json
import os
import subprocess
import sys
import tempfile
import time
from typing import Dict, Any, Optional
import pytest

# Default to the compiled C++ executable as specified by the prompt
EMULATOR_BIN: str = os.environ.get("EMULATOR_BIN", "./build/bin/clothing_app")


class TestAdversarial:
    """Tier 5: Adversarial test cases."""

    @pytest.fixture(autouse=True)
    def setup_temp_dir(self) -> None:
        """Sets up a temporary directory for each test case."""
        with tempfile.TemporaryDirectory() as temp_dir:
            self.temp_dir: str = temp_dir
            yield

    def spawn_interactive(self) -> subprocess.Popen:
        """Spawns an interactive emulator process."""
        cmd = [EMULATOR_BIN, "--headless", "--test-mode", "--interactive"]
        env = os.environ.copy()
        env["ALLOWED_DUMP_DIR"] = self.temp_dir
        proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
        # Read the readiness line
        ready = proc.stdout.readline().strip()
        assert ready == "MOCK_EMULATOR_READY"
        return proc

    def test_cli_ticks_non_numeric_crash(self) -> None:
        """Test that non-numeric --ticks argument is rejected with a clean error message."""
        cmd = [EMULATOR_BIN, "--headless", "--ticks", "abc"]
        env = os.environ.copy()
        env["ALLOWED_DUMP_DIR"] = self.temp_dir
        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5.0,
            env=env,
        )
        assert result.returncode == 1
        assert "Error: Invalid ticks value" in result.stderr

    def test_cli_ticks_overflow_crash(self) -> None:
        """Test that out-of-range/overflow --ticks argument is rejected with a clean error message."""
        cmd = [EMULATOR_BIN, "--headless", "--ticks", "99999999999999999999999999999999"]
        env = os.environ.copy()
        env["ALLOWED_DUMP_DIR"] = self.temp_dir
        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5.0,
            env=env,
        )
        assert result.returncode == 1
        assert "Error: Invalid ticks value" in result.stderr

    def test_headless_negative_ticks_reserve_crash(self) -> None:
        """Test that negative --ticks is rejected with a clean error message."""
        cmd = [EMULATOR_BIN, "--headless", "--ticks", "-5"]
        env = os.environ.copy()
        env["ALLOWED_DUMP_DIR"] = self.temp_dir
        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5.0,
            env=env,
        )
        assert result.returncode == 1
        assert "Error: --ticks cannot be negative" in result.stderr

    def test_interactive_dump_path_traversal(self) -> None:
        """Test path traversal rejection with DUMP_* commands."""
        proc = self.spawn_interactive()
        try:
            parent_dir = os.path.dirname(self.temp_dir)
            target_file = os.path.join(parent_dir, f"traversal_{os.path.basename(self.temp_dir)}.json")
            
            proc.stdin.write(f"DUMP_STATE {target_file}\n")
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            
            assert response.startswith("DUMP_STATE_ERROR")
            assert not os.path.exists(target_file)
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_inject_infinite_stream_dos(self) -> None:
        """Test that injecting /dev/urandom returns INJECT_ERROR cleanly without hanging."""
        proc = self.spawn_interactive()
        try:
            proc.stdin.write("INJECT /dev/urandom\n")
            proc.stdin.flush()
            
            # Read response immediately - should return quickly because of file size limit
            response = proc.stdout.readline().strip()
            assert "INJECT_ERROR" in response
            
            # Verify the process is still running and responsive
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            play_response = proc.stdout.readline().strip()
            assert play_response == "PLAY_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_invalid_json_robustness(self) -> None:
        """Test that injecting invalid JSON structures does not crash the interactive session."""
        proc = self.spawn_interactive()
        try:
            # Malformed JSON (missing closing brace)
            proc.stdin.write('INJECT {"up": true\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"
            
            # Empty JSON braces
            proc.stdin.write('INJECT {}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # Plain brace
            proc.stdin.write('INJECT {\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # Malformed sequence structure
            proc.stdin.write('INJECT [}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # Large frame overflow inside JSON sequence
            proc.stdin.write('INJECT [{"frame": 999999999999999999999999999, "buttons": {}}]\n')
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            # Expecting INJECT_ERROR out_of_range or similar due to std::stoi exception being caught
            assert "INJECT_ERROR" in response
            
            # Check that the process is still alive and responsive after these invalid injections
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"
        finally:
            proc.terminate()
            proc.wait()
