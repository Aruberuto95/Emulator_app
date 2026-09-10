"""Regression checks for native selection and hung subprocess cleanup."""
import subprocess
import sys
import time

import pytest
from emulator_harness import TimedProcess, resolve_binary


def test_missing_native_binary_never_falls_back_to_mock(tmp_path):
    with pytest.raises(RuntimeError, match="Native emulator not found"):
        resolve_binary({}, tmp_path)


def test_mock_requires_explicit_backend(tmp_path):
    assert resolve_binary({"EMULATOR_TEST_BACKEND": "mock"}, tmp_path).endswith("mock_emulator.py")
    script = tmp_path / "mock.py"
    script.write_text("", encoding="utf-8")
    with pytest.raises(RuntimeError, match="Native emulator not found"):
        resolve_binary({"EMULATOR_BIN": str(script)}, tmp_path)


def test_explicit_native_binary_is_resolved(tmp_path):
    binary = tmp_path / "emulator.exe"
    binary.write_bytes(b"fixture")
    assert resolve_binary({"EMULATOR_BIN": str(binary)}, tmp_path) == str(binary.resolve())


def test_hung_response_has_deadline_and_closes_process():
    process = TimedProcess([sys.executable, "-c", "import time; time.sleep(60)"],
                           stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                           text=True, timeout=0.2)
    started = time.monotonic()
    try:
        with pytest.raises(TimeoutError, match="stdout response"):
            process.stdout.readline()
        assert process.poll() is not None
        assert time.monotonic() - started < 5
    finally:
        process.close()


def test_stderr_is_drained_while_waiting_for_response():
    process = TimedProcess([sys.executable, "-c",
                           "import sys; sys.stderr.write('diagnostic\\n' * 10000); print('READY', flush=True)"],
                           stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                           text=True, timeout=5)
    try:
        assert process.stdout.readline().strip() == "READY"
    finally:
        process.close()
