"""Output regressions against the native executable, using only synthetic state."""
import os
from pathlib import Path
import subprocess

import pytest
from emulator_harness import WORKSPACE, resolve_binary, spawn_interactive
from test_e2e import create_mock_rom

pytestmark = pytest.mark.skipif(os.environ.get("EMULATOR_TEST_BACKEND") == "mock",
                                reason="Native frontend regression; mock explicitly selected")


@pytest.mark.parametrize("value", [-1, 10, 1000])
def test_cli_frame_skip_rejects_values_outside_core_range(value):
    result = subprocess.run(
        [resolve_binary(), "--headless", "--ticks", "0", "--frame-skip", str(value)],
        cwd=WORKSPACE, capture_output=True, text=True, timeout=10)
    assert result.returncode != 0
    assert "Frame skip" in result.stderr


@pytest.mark.parametrize("value", [0, 9])
def test_cli_frame_skip_accepts_core_boundaries(tmp_path, value):
    import json
    state_path = tmp_path / "skip.json"
    env = dict(os.environ, ALLOWED_DUMP_DIR=str(tmp_path))
    result = subprocess.run(
        [resolve_binary(), "--headless", "--ticks", "0", "--frame-skip", str(value),
         "--dump-state", str(state_path)],
        cwd=tmp_path, env=env, capture_output=True, text=True, timeout=10)
    assert result.returncode == 0, result.stderr
    assert json.loads(state_path.read_text(encoding="utf-8"))["frame_skip"] == value


def test_protocol_frame_skip_rejects_out_of_range_without_changing_setting(tmp_path):
    import json
    state_path = tmp_path / "skip.json"
    env = dict(os.environ, ALLOWED_DUMP_DIR=str(tmp_path))
    result = subprocess.run(
        [resolve_binary(), "--interactive"],
        input=f"SET_FRAME_SKIP 9\nSET_FRAME_SKIP -1\nSET_FRAME_SKIP 10\nSET_FRAME_SKIP 1000\nDUMP_STATE {state_path}\nEXIT\n",
        cwd=tmp_path, env=env, capture_output=True, text=True, timeout=10)
    assert result.returncode == 0, result.stderr
    assert result.stdout.count("SET_FRAME_SKIP_OK") == 1
    assert result.stdout.count("SET_FRAME_SKIP_ERROR") == 3
    assert json.loads(state_path.read_text(encoding="utf-8"))["frame_skip"] == 9


@pytest.mark.parametrize("with_rom", [False, True])
def test_cli_load_state_failure_stops_before_video_init(tmp_path, with_rom):
    rom = tmp_path / "state-fixture.gba"
    create_mock_rom("GBA", str(rom))
    command = [resolve_binary(), "--load-state", "missing"]
    if with_rom:
        command += ["--rom", str(rom)]
    env = dict(os.environ, ALLOWED_DUMP_DIR=str(tmp_path),
               SDL_VIDEODRIVER="unavailable-for-state-regression")
    # Deliberately use the GUI path: ignoring --load-state would reach SDL_Init
    # and fail for the video driver instead of reporting the missing state.
    result = subprocess.run(command, cwd=tmp_path, env=env, capture_output=True,
                            text=True, timeout=10)
    assert result.returncode != 0
    assert "LOAD_STATE_ERROR" in result.stderr
    assert "SDL could not initialize" not in result.stderr
    assert "LOAD_STATE_OK" not in result.stderr


def test_cli_load_state_restores_progress_and_applies_explicit_speed(tmp_path):
    import json

    rom = tmp_path / "state-fixture.gba"
    create_mock_rom("GBA", str(rom))
    before_path = tmp_path / "before.json"
    after_path = tmp_path / "after.json"
    binary = resolve_binary()
    env = dict(os.environ, ALLOWED_DUMP_DIR=str(tmp_path))
    saved = subprocess.run(
        [binary, "--headless", "--interactive", "--rom", str(rom), "--speed", "2", "--frame-skip", "3"],
        input=f"TICK\nTICK\nSAVE_STATE cli\nDUMP_STATE {before_path}\nEXIT\n",
        cwd=tmp_path, env=env, capture_output=True, text=True, timeout=10)
    assert saved.returncode == 0, saved.stderr
    assert "SAVE_STATE_OK" in saved.stdout
    before = json.loads(before_path.read_text(encoding="utf-8"))
    assert before["ticks"] == 2 and before["cpu_cycles"] > 0
    assert before["speed"] == 2 and before["frame_skip"] == 3

    restored = subprocess.run(
        [binary, "--headless", "--rom", str(rom), "--load-state", "cli", "--ticks", "0",
         "--speed", "5", "--frame-skip", "0", "--pause", "--dump-state", str(after_path)],
        cwd=tmp_path, env=env, capture_output=True, text=True, timeout=10)
    assert restored.returncode == 0, restored.stderr
    assert "LOAD_STATE_OK slot=cli" in restored.stderr
    after = json.loads(after_path.read_text(encoding="utf-8"))
    assert after["ticks"] == before["ticks"]
    assert after["cpu_cycles"] == before["cpu_cycles"]
    assert after["speed"] == 5 and after["frame_skip"] == 0
    assert after["playback_state"] == "pause"


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
