"""Optional native N64 proof using a locally supplied ROM and engine checkpoint.

Commercial game data stays outside the test suite. Copy both inputs so testing
save/reopen cannot overwrite the player's cartridge memory or original state.
"""
import json
import os
from pathlib import Path
import shutil

import pytest

from test_save_persistence import run, slot


@pytest.mark.skipif(
    os.environ.get("EMULATOR_TEST_BACKEND") == "mock"
    or not all(os.environ.get(key) for key in ("N64_TEST_ROM", "N64_TEST_STATE")),
    reason="Requires the native Vulkan core, N64_TEST_ROM and N64_TEST_STATE",
)
def test_n64_resume_restores_scheduler_and_replays_video_audio(tmp_path):
    rom = tmp_path / "game.z64"
    shutil.copyfile(Path(os.environ["N64_TEST_ROM"]), rom)
    shutil.copyfile(Path(os.environ["N64_TEST_STATE"]),
                    tmp_path / "game_savestate_boot.sav")

    prepared = run(tmp_path, rom, "\n".join([
        "LOAD_STATE boot", "PLAY", *("TICK" for _ in range(3)),
        "SAVE_STATE checkpoint", f"DUMP_STATE {tmp_path / 'before.json'}",
        *("TICK" for _ in range(5)), "LOAD_STATE checkpoint",
        f"DUMP_STATE {tmp_path / 'restored.json'}",
    ]))
    assert prepared.returncode == 0, prepared.stderr
    assert "ERROR" not in prepared.stdout, prepared.stdout
    before = json.loads((tmp_path / "before.json").read_text())
    restored = json.loads((tmp_path / "restored.json").read_text())
    for field in ("ticks", "cpu_cycles", "rendered_frames", "console_type"):
        assert restored[field] == before[field], field
    assert before["ticks"] == 3
    assert before["console_type"] == "N64"
    assert slot(tmp_path, rom, "checkpoint").read_bytes().startswith(b"EMUSNAP\0")

    # Fresh processes also prove that serialization contains the GPU history
    # and resampler phase, rather than depending on surviving native resources.
    for name in ("first", "reopened"):
        result = run(tmp_path, rom, "\n".join([
            "LOAD_STATE checkpoint", "PLAY", *("TICK" for _ in range(60)),
            f"DUMP_VIDEO {tmp_path / (name + '.rgb')}",
            f"DUMP_AUDIO {tmp_path / (name + '.pcm')}",
            f"DUMP_STATE {tmp_path / (name + '.json')}", "FLUSH_BATTERY",
        ]))
        assert result.returncode == 0, result.stderr
        assert "ERROR" not in result.stdout, result.stdout
        assert json.loads((tmp_path / f"{name}.json").read_text())["ticks"] == 63
    for suffix in ("rgb", "pcm"):
        expected = (tmp_path / f"first.{suffix}").read_bytes()
        assert len(expected) > 1000 and any(expected), suffix
        assert expected == (tmp_path / f"reopened.{suffix}").read_bytes(), suffix
    assert rom.with_suffix(".sav").read_bytes().startswith(b"N64SAVE1")


@pytest.mark.skipif(
    os.environ.get("EMULATOR_TEST_BACKEND") == "mock" or not os.environ.get("N64_TEST_ROM"),
    reason="Requires the native Vulkan core and N64_TEST_ROM",
)
def test_n64_speed_limit_covers_protocol_cli_and_console_switch(tmp_path):
    import subprocess
    from emulator_harness import resolve_binary
    from test_save_persistence import cartridge

    rom = tmp_path / "game.z64"
    shutil.copyfile(Path(os.environ["N64_TEST_ROM"]), rom)
    gbc = cartridge(tmp_path / "other.gbc", "GBC")
    result = run(tmp_path, rom, "\n".join([
        "SET_SPEED 0.5", "SET_SPEED 5", "SET_SPEED 1garbage",
        f"DUMP_STATE {tmp_path / 'slow.json'}", "SET_SPEED 1", "RESET",
        f"DUMP_STATE {tmp_path / 'reset.json'}",
        f"LOAD_ROM {gbc}", "SET_SPEED 5", f"DUMP_STATE {tmp_path / 'gbc.json'}",
        f"LOAD_ROM {rom}", f"DUMP_STATE {tmp_path / 'n64.json'}",
    ]))
    assert result.returncode == 0, result.stderr
    assert result.stdout.count("SET_SPEED_ERROR") == 2, result.stdout
    assert json.loads((tmp_path / "slow.json").read_text())["speed"] == 0.5
    assert json.loads((tmp_path / "reset.json").read_text())["speed"] == 1
    assert json.loads((tmp_path / "gbc.json").read_text())["speed"] == 5
    assert json.loads((tmp_path / "n64.json").read_text())["speed"] == 1
    rejected = subprocess.run(
        [resolve_binary(), "--headless", "--rom", str(rom), "--speed", "5"],
        cwd=tmp_path, capture_output=True, text=True, timeout=20,
    )
    assert rejected.returncode != 0
    assert "N64 fast-forward is disabled" in rejected.stderr
