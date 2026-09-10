"""Output regressions against the native executable, using only synthetic state."""
import os
from pathlib import Path
import subprocess

import pytest
from emulator_harness import WORKSPACE, resolve_binary, spawn_interactive

pytestmark = pytest.mark.skipif(os.environ.get("EMULATOR_TEST_BACKEND") == "mock",
                                reason="Native frontend regression; mock explicitly selected")


@pytest.mark.parametrize("option", ["--dump-state", "--dump-video", "--dump-audio"])
def test_headless_dump_failure_is_not_success(tmp_path, option):
    target = tmp_path / "missing-parent" / "dump.raw"
    result = subprocess.run([resolve_binary(), "--headless", "--ticks", "1", option, str(target)],
                            cwd=WORKSPACE, capture_output=True, text=True, timeout=10)
    assert result.returncode != 0
    assert "Error:" in result.stderr
    assert not target.exists()


def test_streamed_audio_matches_interactive_history(tmp_path):
    headless = tmp_path / "headless.raw"
    interactive = tmp_path / "interactive.raw"
    binary = resolve_binary()
    result = subprocess.run([binary, "--headless", "--ticks", "12", "--dump-audio", str(headless)],
                            cwd=WORKSPACE, capture_output=True, text=True, timeout=10)
    assert result.returncode == 0, result.stderr
    process = spawn_interactive(binary, tmp_path)
    try:
        for _ in range(12):
            process.stdin.write("TICK\n")
            process.stdin.flush()
            assert process.stdout.readline().startswith("TICK_OK")
        process.stdin.write(f"DUMP_AUDIO {interactive}\n")
        process.stdin.flush()
        assert process.stdout.readline().strip() == "DUMP_AUDIO_OK"
    finally:
        process.close()
    assert headless.stat().st_size > 0
    assert headless.read_bytes() == interactive.read_bytes()


def test_headless_without_audio_dump_keeps_state_identical(tmp_path):
    states = []
    for dump_audio in (False, True):
        target = tmp_path / f"state-{dump_audio}.json"
        command = [resolve_binary(), "--headless", "--ticks", "30", "--dump-state", str(target)]
        if dump_audio:
            command += ["--dump-audio", str(tmp_path / "audio.raw")]
        result = subprocess.run(command, cwd=WORKSPACE, capture_output=True, text=True, timeout=10)
        assert result.returncode == 0, result.stderr
        import json
        state = json.loads(target.read_text(encoding="utf-8"))
        state.pop("video_buffer_addr", None)
        state.pop("audio_buffer_addr", None)
        states.append(state)
    assert states[0] == states[1]


@pytest.mark.skipif(os.name != "nt", reason="Peak RSS regression uses Windows process counters")
def test_headless_without_dump_does_not_retain_audio():
    import ctypes
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
            (name, ctypes.c_size_t) for name in (
                "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
                "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage",
                "PagefileUsage", "PeakPagefileUsage")]

    get_memory = ctypes.WinDLL("psapi", use_last_error=True).GetProcessMemoryInfo
    get_memory.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    get_memory.restype = wintypes.BOOL

    def peak(ticks):
        process = subprocess.Popen([resolve_binary(), "--headless", "--ticks", str(ticks)],
                                   cwd=WORKSPACE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            stdout, stderr = process.communicate(timeout=30)
            assert process.returncode == 0, stderr.decode(errors="replace")
            counters = Counters()
            counters.cb = ctypes.sizeof(counters)
            assert get_memory(int(process._handle), ctypes.byref(counters), counters.cb), ctypes.get_last_error()
            return counters.PeakWorkingSetSize
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)

    # The previous unconditional audio history retained about 59 MB at 20,000 ticks.
    # Use a wide 32 MiB allowance for unrelated allocator/startup differences.
    assert peak(20000) - peak(200) < 32 * 1024 * 1024


def test_interactive_injection_checks_file_type_not_its_name(tmp_path):
    path = tmp_path / "random-input.json"
    path.write_text('{"right": true}', encoding="utf-8")
    process = spawn_interactive(resolve_binary(), tmp_path)
    try:
        for target in (tmp_path / "missing.json", tmp_path):
            process.stdin.write(f"INJECT {target}\n")
            process.stdin.flush()
            assert process.stdout.readline().startswith("INJECT_ERROR")
        if hasattr(os, "mkfifo"):
            fifo = tmp_path / "input.fifo"
            os.mkfifo(fifo)
            process.stdin.write(f"INJECT {fifo}\n")
            process.stdin.flush()
            assert process.stdout.readline().startswith("INJECT_ERROR")
        process.stdin.write(f"INJECT {path}\n")
        process.stdin.flush()
        assert process.stdout.readline().strip() == "INJECT_OK"
    finally:
        process.close()
