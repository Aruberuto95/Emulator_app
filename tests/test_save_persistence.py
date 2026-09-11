"""Native save/reopen regressions. Every cartridge and save is synthetic."""
import json
import os
from pathlib import Path
import struct
import subprocess

import pytest
from emulator_harness import resolve_binary
from test_e2e import create_mock_rom, GBA_NINTENDO_LOGO

pytestmark = pytest.mark.skipif(os.environ.get("EMULATOR_TEST_BACKEND") == "mock",
                                reason="Requires the native core")

SIZES = {"GBC": 32768, "GBA": 131072, "NDS": 524288}
FIELDS = {"GBC": "gbc_mmu_mbc_ram", "GBA": "gba_flash_data"}


def cartridge(path, console):
    path.parent.mkdir(parents=True, exist_ok=True)
    if console != "NDS":
        create_mock_rom(console, str(path))
    else:
        data = bytearray(528)
        data[:12] = b"TESTNDSROM\0\0"
        data[12:16] = b"TEST"
        struct.pack_into("<8I", data, 0x20, 512, 0x02000000, 0x02000000, 4,
                         516, 0x03800000, 0x03800000, 4)
        data[0xC0:0xC0 + 156] = GBA_NINTENDO_LOGO
        struct.pack_into("<II", data, 512, 0xEAFFFFFE, 0xEAFFFFFE)
        path.write_bytes(data)
    return path


def run(base, rom, commands, **environment):
    env = dict(os.environ, ALLOWED_DUMP_DIR=str(base))
    env.pop("MOCK_DISK_FULL", None)
    env.update(environment)
    return subprocess.run([resolve_binary(), "--interactive", "--rom", str(rom)],
                          input=commands + "\nEXIT\n", cwd=base, env=env,
                          text=True, capture_output=True, timeout=20)


def slot(base, rom, name):
    files = list(base.glob(f"{rom.stem}*_savestate_{name}.sav"))
    assert len(files) == 1, files
    return files[0]


@pytest.mark.parametrize("console", SIZES)
def test_restored_battery_survives_exit_and_reopen(tmp_path, console):
    rom = cartridge(tmp_path / ("game." + console.lower()), console)
    battery = rom.with_suffix(".sav")
    battery.write_bytes(bytes([0x5A]) * SIZES[console])
    assert "SAVE_STATE_OK" in run(tmp_path, rom, "SAVE_STATE old").stdout
    battery.write_bytes(bytes([0xA5]) * SIZES[console])
    restored = run(tmp_path, rom, "LOAD_STATE old")
    assert restored.returncode == 0, restored.stderr
    assert "LOAD_STATE_OK" in restored.stdout
    assert battery.read_bytes()[:SIZES[console]] == bytes([0x5A]) * SIZES[console]
    reopened = run(tmp_path, rom, "SAVE_STATE reopened")
    assert "SAVE_STATE_OK" in reopened.stdout
    if console in FIELDS:
        state = json.loads(slot(tmp_path, rom, "reopened").read_text())
        assert state[FIELDS[console]] == "5a" * SIZES[console]


@pytest.mark.parametrize("console", SIZES)
def test_same_name_roms_have_separate_slots_and_wrong_rom_is_refused(tmp_path, console):
    a = cartridge(tmp_path / "one" / ("same." + console.lower()), console)
    b = cartridge(tmp_path / "two" / a.name, console)
    data = bytearray(b.read_bytes())
    data[-1] ^= 1  # Different ROM; same title, gamecode and file length.
    b.write_bytes(data)
    assert "SAVE_STATE_OK" in run(tmp_path, a, "SAVE_STATE 0").stdout
    first = slot(tmp_path, a, "0")
    before = first.read_bytes()
    assert "LOAD_STATE_ERROR" in run(tmp_path, b, "LOAD_STATE 0").stdout
    assert "SAVE_STATE_OK" in run(tmp_path, b, "SAVE_STATE 0").stdout
    files = list(tmp_path.glob("same*_savestate_0.sav"))
    assert len(files) == 2
    second = next(p for p in files if p != first)
    assert first.read_bytes() == before
    second.write_bytes(before)  # Renaming a slot must not bypass ROM identity.
    assert "LOAD_STATE_ERROR" in run(tmp_path, b, "LOAD_STATE 0").stdout


@pytest.mark.parametrize("console", ["GBC", "GBA"])
@pytest.mark.parametrize("damage", ["truncate", "bitflip", "legacy_truncate", "missing_version"])
def test_damaged_state_is_refused_before_changing_machine(tmp_path, console, damage):
    rom = cartridge(tmp_path / ("game." + console.lower()), console)
    assert "SAVE_STATE_OK" in run(tmp_path, rom, "SAVE_STATE bad").stdout
    path = slot(tmp_path, rom, "bad")
    text = path.read_text()
    if damage == "missing_version":
        state = json.loads(text)
        del state["state_version"]
        text = json.dumps(state)
    elif damage == "bitflip":
        state = json.loads(text)
        state[FIELDS[console]] = "aa" + state[FIELDS[console]][2:]
        text = json.dumps(state)
    else:
        if damage == "legacy_truncate":
            state = json.loads(text)
            for key in ("state_version", "state_hash", "rom_hash"):
                state.pop(key, None)
            text = json.dumps(state)
        text = text[:-8]
    path.write_text(text)
    result = run(tmp_path, rom, "SET_SPEED 2\nSAVE_STATE before\nLOAD_STATE bad\nSAVE_STATE after")
    assert "LOAD_STATE_ERROR" in result.stdout
    assert slot(tmp_path, rom, "before").read_bytes() == slot(tmp_path, rom, "after").read_bytes()


def writing_gbc(path):
    cartridge(path, "GBC")
    data = bytearray(path.read_bytes())
    data.extend(bytes(0x8000 - len(data)))
    data[0x100:0x103] = bytes.fromhex("C3 50 01")
    code = bytes.fromhex("F3 3E 0A EA 00 00 3E 5A EA 00 A0 76 18 FD")
    data[0x150:0x150 + len(code)] = code
    path.write_bytes(data)
    return path


def test_failed_battery_write_is_reported_and_rom_switch_keeps_pending_data(tmp_path):
    a = writing_gbc(tmp_path / "writer.gbc")
    b = cartridge(tmp_path / "other.gba", "GBA")
    old = bytes([0x33]) * 32768
    a.with_suffix(".sav").write_bytes(old)
    result = run(tmp_path, a, f"PLAY\nTICK\nLOAD_ROM {b}\nSAVE_STATE pending",
                 MOCK_DISK_FULL="1")
    assert result.returncode != 0
    assert "BATTERY_SAVE_ERROR" in result.stderr
    assert "LOAD_ROM_ERROR" in result.stdout
    assert a.with_suffix(".sav").read_bytes() == old
    assert not b.with_suffix(".sav").exists()


@pytest.mark.parametrize("console", SIZES)
def test_battery_read_failure_does_not_replace_running_rom(tmp_path, console):
    a = cartridge(tmp_path / "first.gbc", "GBC")
    b = cartridge(tmp_path / ("unreadable." + console.lower()), console)
    b.with_suffix(".sav").mkdir()  # Portable read failure, no permission tricks.
    result = run(tmp_path, a, f"LOAD_ROM {b}\nSAVE_STATE still_first")
    assert "LOAD_ROM_ERROR" in result.stdout
    assert "SAVE_STATE_OK" in result.stdout
    assert slot(tmp_path, a, "still_first").exists()


@pytest.mark.parametrize("console", SIZES)
def test_failed_flush_can_be_retried_without_losing_restored_battery(tmp_path, console):
    rom = cartridge(tmp_path / ("retry." + console.lower()), console)
    battery = rom.with_suffix(".sav")
    expected = bytes([0x5A]) * SIZES[console]
    battery.write_bytes(expected)
    assert "SAVE_STATE_OK" in run(tmp_path, rom, "SAVE_STATE retry").stdout
    process = subprocess.Popen([resolve_binary(), "--interactive"], cwd=tmp_path,
                               env=dict(os.environ, ALLOWED_DUMP_DIR=str(tmp_path)),
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True)

    def command(value):
        process.stdin.write(value + "\n")
        process.stdin.flush()
        return process.stdout.readline().strip()

    try:
        assert process.stdout.readline().strip() == "MOCK_EMULATOR_READY"
        assert command(f"LOAD_ROM {rom}") == "LOAD_ROM_OK"
        assert command("LOAD_STATE retry") == "LOAD_STATE_OK"
        battery.unlink()
        battery.mkdir()  # Force replacement to fail after the ROM was loaded.
        assert command("FLUSH_BATTERY").startswith("BATTERY_SAVE_ERROR")
        assert command("SAVE_STATE still_open") == "SAVE_STATE_OK"
        battery.rmdir()
        assert command("FLUSH_BATTERY") == "FLUSH_BATTERY_OK"
        assert battery.read_bytes()[:SIZES[console]] == expected
        assert command("FLUSH_BATTERY") == "FLUSH_BATTERY_OK"
        assert command("EXIT") == "EXIT_OK"
        assert process.wait(timeout=10) == 0
    finally:
        if process.poll() is None:
            process.terminate()
        process.wait(timeout=10)


@pytest.mark.parametrize("console", SIZES)
def test_legacy_named_slots_remain_loadable(tmp_path, console):
    rom = cartridge(tmp_path / ("legacy." + console.lower()), console)
    assert "SAVE_STATE_OK" in run(tmp_path, rom, "SAVE_STATE legacy").stdout
    path = slot(tmp_path, rom, "legacy")
    data = path.read_bytes()
    if console == "NDS":
        data = data[:8] + struct.pack("<I", 3) + data[12:37] + data[45:]
    else:
        state = json.loads(data)
        for key in ("state_version", "state_hash", "rom_hash"):
            del state[key]
        data = json.dumps(state).encode()
    path.unlink()
    legacy = tmp_path / f"{rom.stem}_savestate_legacy.sav"
    legacy.write_bytes(data)
    assert "LOAD_STATE_OK" in run(tmp_path, rom, "LOAD_STATE legacy").stdout
    assert legacy.read_bytes() == data
