"""E2E Test Suite for the Universal GBC and GBA Emulator.

This suite implements Tiers 1 through 4 of the E2E test design, verifying
ROM scanning, header parsing, dynamic resolution, button mappings (including L/R),
speed control, frame skipping, atomic savestates, and CPU/APU execution mocks.
"""

from dataclasses import dataclass
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Dict, List, Optional, Any
import pytest

# Path to the mock emulator script
MOCK_EMULATOR_PATH: str = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "mock_emulator.py"
)
EMULATOR_BIN: str = os.environ.get("EMULATOR_BIN", MOCK_EMULATOR_PATH)


@dataclass(frozen=True)
class ButtonState:
    """Represents digital button inputs for a specific frame."""

    up: bool = False
    down: bool = False
    left: bool = False
    right: bool = False
    a: bool = False
    b: bool = False
    start: bool = False
    select: bool = False
    l: bool = False  # noqa: E741
    r: bool = False

    def to_dict(self) -> Dict[str, bool]:
        """Convert button state to a dictionary structure.

        Returns:
            Dict[str, bool]: Dictionary of button states.
        """
        return {
            "up": self.up,
            "down": self.down,
            "left": self.left,
            "right": self.right,
            "a": self.a,
            "b": self.b,
            "start": self.start,
            "select": self.select,
            "l": self.l,
            "r": self.r,
        }


class InputSequenceGenerator:
    """Generates and writes input sequences to JSON for emulator consumption."""

    def __init__(self) -> None:
        """Initializes the input sequence generator."""
        self._frames: Dict[int, ButtonState] = {}

    def set_frame_input(self, frame: int, buttons: ButtonState) -> None:
        """Sets the input state for a specific frame index.

        Args:
            frame (int): The frame index.
            buttons (ButtonState): The button state.
        """
        self._frames[frame] = buttons

    def write_to_file(self, file_path: str) -> None:
        """Writes the accumulated input sequence to a JSON file.

        Args:
            file_path (str): The output JSON file path.
        """
        sequence_data = []
        for frame, state in sorted(self._frames.items()):
            sequence_data.append({
                "frame": frame,
                "buttons": state.to_dict()
            })

        with open(file_path, "w", encoding="utf-8") as f:
            json.dump(sequence_data, f, indent=2)


@dataclass(frozen=True)
class EmulatorRunResult:
    """Contains results and file paths returned by an emulator execution run."""

    return_code: int
    stdout: str
    stderr: str
    video_path: Optional[str]
    audio_path: Optional[str]
    state_path: Optional[str]

    @property
    def is_success(self) -> bool:
        """Returns True if the binary exited cleanly.

        Returns:
            bool: True if return code is 0.
        """
        return self.return_code == 0

    def parse_state(self) -> Dict[str, Any]:
        """Parses and returns the state JSON dumped by the emulator.

        Returns:
            Dict[str, Any]: The parsed state dictionary.
        """
        if not self.state_path or not os.path.exists(self.state_path):
            raise ValueError("State dump was not requested or failed to generate.")
        with open(self.state_path, "r", encoding="utf-8") as f:
            return json.load(f)

    def read_video_buffer(self) -> bytes:
        """Reads the raw video buffer bytes.

        Returns:
            bytes: The raw video buffer.
        """
        if not self.video_path or not os.path.exists(self.video_path):
            raise ValueError("Video dump was not requested or failed to generate.")
        with open(self.video_path, "rb") as f:
            return f.read()

    def read_audio_buffer(self) -> bytes:
        """Reads the raw audio buffer bytes.

        Returns:
            bytes: The raw audio buffer.
        """
        if not self.audio_path or not os.path.exists(self.audio_path):
            raise ValueError("Audio dump was not requested or failed to generate.")
        with open(self.audio_path, "rb") as f:
            return f.read()


class EmulatorProcessRunner:
    """Handles invoking the emulator binary and capturing output assets."""

    def __init__(self, binary_path: str, temp_dir: str) -> None:
        """Initializes the emulator process runner.

        Args:
            binary_path (str): Path to the emulator executable.
            temp_dir (str): Temporary directory to write dump files.
        """
        self._binary_path: str = binary_path
        self._temp_dir: str = temp_dir
        self._run_count: int = 0

    def run(
        self,
        ticks: int,
        input_file: Optional[str] = None,
        dump_video: bool = True,
        dump_audio: bool = True,
        dump_state: bool = True,
        rom: Optional[str] = None,
        speed: Optional[float] = None,
        frame_skip: Optional[int] = None,
        extra_args: Optional[List[str]] = None,
    ) -> EmulatorRunResult:
        """Executes the emulator and retrieves execution artifacts."""
        self._run_count += 1
        suffix = f"_{self._run_count}"
        video_path = os.path.join(self._temp_dir, f"video{suffix}.raw") if dump_video else None
        audio_path = os.path.join(self._temp_dir, f"audio{suffix}.raw") if dump_audio else None
        state_path = os.path.join(self._temp_dir, f"state{suffix}.json") if dump_state else None

        if self._binary_path.endswith(".py"):
            cmd = [sys.executable, self._binary_path]
        else:
            cmd = [self._binary_path]
        cmd.extend([
            "--headless",
            "--test-mode",
            "--ticks",
            str(ticks),
        ])

        if input_file:
            cmd.extend(["--input-inject", input_file])
        if video_path:
            cmd.extend(["--dump-video", video_path])
        if audio_path:
            cmd.extend(["--dump-audio", audio_path])
        if state_path:
            cmd.extend(["--dump-state", state_path])
        if rom:
            cmd.extend(["--rom", rom])
        if speed is not None:
            cmd.extend(["--speed", str(speed)])
        if frame_skip is not None:
            cmd.extend(["--frame-skip", str(frame_skip)])
        if extra_args:
            cmd.extend(extra_args)

        env = os.environ.copy()
        env["ALLOWED_DUMP_DIR"] = self._temp_dir
        process = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10.0,
            check=False,
            env=env,
        )

        return EmulatorRunResult(
            return_code=process.returncode,
            stdout=process.stdout,
            stderr=process.stderr,
            video_path=video_path if video_path and os.path.exists(video_path) else None,
            audio_path=audio_path if audio_path and os.path.exists(audio_path) else None,
            state_path=state_path if state_path and os.path.exists(state_path) else None,
        )


class InteractiveEmulatorSession:
    """Manages an interactive session with the emulator process over stdin/stdout."""

    def __init__(self, process: subprocess.Popen) -> None:
        """Initializes the interactive session."""
        self._process: subprocess.Popen = process

    def send_command(self, cmd: str) -> str:
        """Sends a command to the process and reads the response line."""
        if self._process.stdin is None or self._process.stdout is None:
            raise RuntimeError("Subprocess pipes are not initialized.")
        self._process.stdin.write(f"{cmd}\n")
        self._process.stdin.flush()
        response = self._process.stdout.readline().strip()
        return response

    def close(self) -> None:
        """Closes the interactive session and terminates the subprocess."""
        try:
            self.send_command("EXIT")
        except Exception:
            pass
        self._process.terminate()
        self._process.wait()


class TestBase:
    """Base class for test cases containing common fixtures and helpers."""

    @pytest.fixture(autouse=True)
    def setup_temp_dir(self) -> None:
        """Sets up a temporary directory for each test case."""
        workspace_dir = "/Users/a.rudolph/Proyectos Albert/clothing_app"
        local_temp = os.path.join(workspace_dir, "tests", "tmp")
        os.makedirs(local_temp, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=local_temp) as temp_dir:
            self.temp_dir: str = temp_dir
            self.runner: EmulatorProcessRunner = EmulatorProcessRunner(
                EMULATOR_BIN, self.temp_dir
            )
            yield

    def spawn_interactive(self, extra_args: Optional[List[str]] = None) -> InteractiveEmulatorSession:
        """Spawns an interactive emulator process."""
        if EMULATOR_BIN.endswith(".py"):
            cmd = [sys.executable, EMULATOR_BIN]
        else:
            cmd = [EMULATOR_BIN]
        cmd.extend([
            "--headless",
            "--test-mode",
            "--interactive",
        ])
        if extra_args:
            cmd.extend(extra_args)
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
        if proc.stdout is None:
            raise RuntimeError("Failed to redirect stdout.")
        ready = proc.stdout.readline().strip()
        assert ready == "MOCK_EMULATOR_READY"
        return InteractiveEmulatorSession(proc)


# Logo bytes for GBC and GBA
GBC_NINTENDO_LOGO = bytes([
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E
])

GBA_NINTENDO_LOGO = bytes([
    0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A, 0xA2, 0x21, 0x3D, 0x84, 0x82, 0x0A, 0x84, 0xE4, 0x09, 0xAD,
    0x11, 0x24, 0x8B, 0x98, 0xC0, 0x81, 0x7F, 0x21, 0xA3, 0x52, 0xBE, 0x19, 0x93, 0x09, 0xCE, 0x20,
    0x10, 0x46, 0x4A, 0x4A, 0xF8, 0x27, 0x31, 0xEC, 0x58, 0xC7, 0xE8, 0x33, 0x82, 0xE3, 0xCE, 0xBF,
    0x85, 0xF4, 0xDF, 0x94, 0xCE, 0x4B, 0x09, 0xC1, 0x94, 0x56, 0x8A, 0xC0, 0x13, 0x72, 0xA7, 0xFC,
    0x9F, 0x84, 0x4D, 0x73, 0xA3, 0xCA, 0x9A, 0x61, 0x58, 0x97, 0xA3, 0x27, 0xFC, 0x03, 0x98, 0x76,
    0x23, 0x1D, 0xC7, 0x61, 0x03, 0x04, 0xAE, 0x56, 0xBF, 0x38, 0x84, 0x00, 0x40, 0xA7, 0x0E, 0xFD,
    0xFF, 0x52, 0xFE, 0x03, 0x6F, 0x95, 0x30, 0xF1, 0x97, 0xFB, 0xC0, 0x85, 0x60, 0xD6, 0x80, 0x25,
    0xA9, 0x63, 0xBE, 0x03, 0x01, 0x4E, 0x38, 0xE2, 0xF9, 0xA2, 0x34, 0xFF, 0xBB, 0x3E, 0x03, 0x44,
    0x78, 0x00, 0x90, 0xCB, 0x88, 0x11, 0x3A, 0x94, 0x65, 0xC0, 0x7C, 0x63, 0x87, 0xF0, 0x3C, 0xAF,
    0xD6, 0x25, 0xE4, 0x8B, 0x38, 0x0A, 0xAC, 0x72, 0x21, 0xD4, 0xF8, 0x07
])

def create_mock_rom(console_type: str, path: str, corrupt_logo: bool = False, corrupt_checksum: bool = False, truncated: bool = False, non_ascii_title: bool = False) -> None:
    """Helper to generate GBC and GBA ROM files with valid or corrupt headers."""
    if console_type == "GBC":
        size = 0x150
        if truncated:
            size = 0x100
        data = bytearray(size)
        
        logo = bytearray(GBC_NINTENDO_LOGO)
        if corrupt_logo:
            logo[0] ^= 0xFF
        if len(data) >= 0x104 + len(logo):
            data[0x104 : 0x104 + len(logo)] = logo
        
        if len(data) >= 0x143:
            data[0x143] = 0x80
        
        if non_ascii_title and len(data) >= 0x134 + 7:
            title = "Pokémon".encode("latin1")
            data[0x134 : 0x134 + len(title)] = title
        
        if len(data) >= 0x14E:
            checksum = 0
            for i in range(0x134, 0x14D):
                checksum = (checksum - data[i] - 1) & 0xFF
            if corrupt_checksum:
                checksum = (checksum + 1) & 0xFF
            data[0x14D] = checksum
        
    else: # GBA
        size = 0x200
        if truncated:
            size = 0xB0
        data = bytearray(size)
        
        logo = bytearray(GBA_NINTENDO_LOGO)
        if corrupt_logo:
            logo[0] ^= 0xFF
        if len(data) >= 0x004 + len(logo):
            data[0x004 : 0x004 + len(logo)] = logo
            
        if len(data) >= 0xB2:
            data[0xB2] = 0x96
            
        if non_ascii_title and len(data) >= 0xA0 + 7:
            title = "Pokémon".encode("latin1")
            data[0xA0 : 0xA0 + len(title)] = title
            
        if len(data) >= 0xBD:
            checksum = 0
            for i in range(0xA0, 0xBD):
                checksum = (checksum - data[i] - 1) & 0xFF
            if corrupt_checksum:
                checksum = (checksum + 1) & 0xFF
            data[0xBD] = checksum
            
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)


# ==============================================================================
# TIER 1: Feature Coverage (40 cases)
# ==============================================================================

class TestTier1FeatureCoverage(TestBase):
    """Tier 1: Feature Coverage E2E Tests."""

    # --- Feature 1: ROM Scanning & Path Safety (T1.1 - T1.5) ---

    def test_t1_1_rom_directory_scanning(self) -> None:
        """T1.1: ROM Directory Scanning."""
        rom_dir = os.path.join(self.temp_dir, "roms")
        create_mock_rom("GBC", os.path.join(rom_dir, "game1.gbc"))
        create_mock_rom("GBA", os.path.join(rom_dir, "game2.gba"))

        session = self.spawn_interactive()
        try:
            resp = session.send_command(f"SCAN_ROMS {rom_dir}")
            assert resp.startswith("SCAN_ROMS_OK")
            data = json.loads(resp.split(maxsplit=1)[1])
            paths = [os.path.basename(item["path"]) for item in data]
            assert "game1.gbc" in paths
            assert "game2.gba" in paths
        finally:
            session.close()

    def test_t1_2_relative_path_safety(self) -> None:
        """T1.2: Relative Path Safety."""
        rel_dir = "tests/test_temp_t1_2"
        os.makedirs(rel_dir, exist_ok=True)
        rel_rom = os.path.join(rel_dir, "game.gbc")
        create_mock_rom("GBC", rel_rom)
        try:
            session = self.spawn_interactive()
            try:
                resp = session.send_command(f"LOAD_ROM {rel_rom}")
                assert resp == "LOAD_ROM_OK"
            finally:
                session.close()
        finally:
            if os.path.exists(rel_dir):
                shutil.rmtree(rel_dir)

    def test_t1_3_path_traversal_rejection(self) -> None:
        """T1.3: Path Traversal Rejection."""
        session = self.spawn_interactive()
        try:
            resp = session.send_command("LOAD_ROM ../../invalid.gbc")
            assert "LOAD_ROM_ERROR Path traversal detected" in resp
        finally:
            session.close()

    def test_t1_4_symlink_traversal_rejection(self) -> None:
        """T1.4: Symlink Traversal Rejection."""
        sym_path = os.path.join(self.temp_dir, "bad_symlink.gbc")
        try:
            os.symlink("/etc/hosts", sym_path)
        except Exception:
            pytest.skip("Symlink not supported")

        session = self.spawn_interactive()
        try:
            resp = session.send_command(f"LOAD_ROM {sym_path}")
            assert "LOAD_ROM_ERROR Symlink points outside allowed directory" in resp or "LOAD_ROM_ERROR Path traversal detected" in resp
        finally:
            session.close()

    def test_t1_5_concurrent_scans(self) -> None:
        """T1.5: Concurrent Scans."""
        rom_dir = os.path.join(self.temp_dir, "roms")
        create_mock_rom("GBC", os.path.join(rom_dir, "game.gbc"))
        s1 = self.spawn_interactive()
        s2 = self.spawn_interactive()
        try:
            resp1 = s1.send_command(f"SCAN_ROMS {rom_dir}")
            resp2 = s2.send_command(f"SCAN_ROMS {rom_dir}")
            assert resp1.startswith("SCAN_ROMS_OK")
            assert resp2.startswith("SCAN_ROMS_OK")
        finally:
            s1.close()
            s2.close()

    # --- Feature 2: Header Parsing (T2.1 - T2.5) ---

    def test_t2_1_valid_gbc_header(self) -> None:
        """T2.1: Valid GBC Header."""
        rom_path = os.path.join(self.temp_dir, "valid.gbc")
        create_mock_rom("GBC", rom_path)
        session = self.spawn_interactive()
        try:
            assert session.send_command(f"LOAD_ROM {rom_path}") == "LOAD_ROM_OK"
        finally:
            session.close()

    def test_t2_2_valid_gba_header(self) -> None:
        """T2.2: Valid GBA Header."""
        rom_path = os.path.join(self.temp_dir, "valid.gba")
        create_mock_rom("GBA", rom_path)
        session = self.spawn_interactive()
        try:
            assert session.send_command(f"LOAD_ROM {rom_path}") == "LOAD_ROM_OK"
        finally:
            session.close()

    def test_t2_3_bad_nintendo_logo_rejection(self) -> None:
        """T2.3: Bad Nintendo Logo Rejection."""
        rom_path = os.path.join(self.temp_dir, "bad_logo.gbc")
        create_mock_rom("GBC", rom_path, corrupt_logo=True)
        session = self.spawn_interactive()
        try:
            resp = session.send_command(f"LOAD_ROM {rom_path}")
            assert "LOAD_ROM_ERROR Invalid Nintendo logo" in resp
        finally:
            session.close()

    def test_t2_4_bad_checksum_rejection(self) -> None:
        """T2.4: Bad Checksum Rejection."""
        rom_path = os.path.join(self.temp_dir, "bad_checksum.gbc")
        create_mock_rom("GBC", rom_path, corrupt_checksum=True)
        session = self.spawn_interactive()
        try:
            resp = session.send_command(f"LOAD_ROM {rom_path}")
            assert "LOAD_ROM_ERROR GBC header checksum mismatch" in resp
        finally:
            session.close()

    def test_t2_5_truncated_rom_handling(self) -> None:
        """T2.5: Truncated ROM Handling."""
        rom_path = os.path.join(self.temp_dir, "trunc.gbc")
        create_mock_rom("GBC", rom_path, truncated=True)
        session = self.spawn_interactive()
        try:
            resp = session.send_command(f"LOAD_ROM {rom_path}")
            assert "LOAD_ROM_ERROR Truncated ROM" in resp
        finally:
            session.close()

    # --- Feature 3: Dynamic GBC/GBA Resolution (T3.1 - T3.5) ---

    def test_t3_1_gbc_resolution(self) -> None:
        """T3.1: GBC Resolution (160x144, 69120 bytes)."""
        rom_path = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", rom_path)
        result = self.runner.run(ticks=1, rom=rom_path)
        assert result.is_success
        state = result.parse_state()
        assert state["console_type"] == "GBC"
        assert len(result.read_video_buffer()) == 69120

    def test_t3_2_gba_resolution(self) -> None:
        """T3.2: GBA Resolution (240x160, 115200 bytes)."""
        rom_path = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", rom_path)
        result = self.runner.run(ticks=1, rom=rom_path)
        assert result.is_success
        state = result.parse_state()
        assert state["console_type"] == "GBA"
        assert len(result.read_video_buffer()) == 115200

    def test_t3_3_dynamic_switch_gbc_to_gba(self) -> None:
        """T3.3: Dynamic Switch GBC to GBA."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBC", gbc_rom)
        create_mock_rom("GBA", gba_rom)

        session = self.spawn_interactive()
        try:
            session.send_command(f"LOAD_ROM {gbc_rom}")
            state_p1 = os.path.join(self.temp_dir, "state1.json")
            session.send_command(f"DUMP_STATE {state_p1}")
            with open(state_p1, "r") as f:
                s1 = json.load(f)
            assert s1["console_type"] == "GBC"

            session.send_command(f"LOAD_ROM {gba_rom}")
            state_p2 = os.path.join(self.temp_dir, "state2.json")
            session.send_command(f"DUMP_STATE {state_p2}")
            with open(state_p2, "r") as f:
                s2 = json.load(f)
            assert s2["console_type"] == "GBA"
        finally:
            session.close()

    def test_t3_4_dynamic_switch_gba_to_gbc(self) -> None:
        """T3.4: Dynamic Switch GBA to GBC."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBC", gbc_rom)
        create_mock_rom("GBA", gba_rom)

        session = self.spawn_interactive()
        try:
            session.send_command(f"LOAD_ROM {gba_rom}")
            state_p1 = os.path.join(self.temp_dir, "state1.json")
            session.send_command(f"DUMP_STATE {state_p1}")
            with open(state_p1, "r") as f:
                s1 = json.load(f)
            assert s1["console_type"] == "GBA"

            session.send_command(f"LOAD_ROM {gbc_rom}")
            state_p2 = os.path.join(self.temp_dir, "state2.json")
            session.send_command(f"DUMP_STATE {state_p2}")
            with open(state_p2, "r") as f:
                s2 = json.load(f)
            assert s2["console_type"] == "GBC"
        finally:
            session.close()

    def test_t3_5_coordinate_alignment(self) -> None:
        """T3.5: Coordinate Alignment centering."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBC", gbc_rom)
        create_mock_rom("GBA", gba_rom)

        session = self.spawn_interactive()
        try:
            session.send_command(f"LOAD_ROM {gbc_rom}")
            state_p1 = os.path.join(self.temp_dir, "state1.json")
            session.send_command(f"DUMP_STATE {state_p1}")
            with open(state_p1, "r") as f:
                s1 = json.load(f)
            assert s1["player_x"] == 80
            assert s1["player_y"] == 72

            session.send_command(f"LOAD_ROM {gba_rom}")
            state_p2 = os.path.join(self.temp_dir, "state2.json")
            session.send_command(f"DUMP_STATE {state_p2}")
            with open(state_p2, "r") as f:
                s2 = json.load(f)
            assert s2["player_x"] == 120
            assert s2["player_y"] == 80
        finally:
            session.close()

    # --- Feature 4: Controller Button Mappings (T4.1 - T4.5) ---

    def test_t4_1_gbc_inputs(self) -> None:
        """T4.1: GBC Inputs."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            buttons = {"up": True, "down": True, "left": True, "right": True, "a": True, "b": True, "start": True, "select": True}
            assert session.send_command(f"INJECT {json.dumps(buttons)}") == "INJECT_OK"
            session.send_command("TICK")
            
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            for btn in buttons:
                assert state["buttons"][btn] is True
        finally:
            session.close()

    def test_t4_2_gba_inputs_with_l_r(self) -> None:
        """T4.2: GBA Inputs (with L/R)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            buttons = {"l": True, "r": True}
            assert session.send_command(f"INJECT {json.dumps(buttons)}") == "INJECT_OK"
            session.send_command("TICK")
            
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["buttons"]["l"] is True
            assert state["buttons"]["r"] is True
        finally:
            session.close()

    def test_t4_3_socd_neutralization(self) -> None:
        """T4.3: SOCD Neutralization (L+R)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            # Inject start to enter gameplay
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")

            # Neutral left + right
            session.send_command(f"INJECT {json.dumps({'left': True, 'right': True})}")
            session.send_command("TICK")
            
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            # player_x remains 80
            assert state["player_x"] == 80
        finally:
            session.close()

    def test_t4_4_mash_combination(self) -> None:
        """T4.4: Mash Combination (A+B+L+R+START)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            mash = {"a": True, "b": True, "l": True, "r": True, "start": True}
            assert session.send_command(f"INJECT {json.dumps(mash)}") == "INJECT_OK"
            session.send_command("TICK")
            
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            for k in mash:
                assert state["buttons"][k] is True
        finally:
            session.close()

    def test_t4_5_release_latency(self) -> None:
        """T4.5: Release Latency."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'a': True})}")
            session.send_command("TICK")
            
            st_path = os.path.join(self.temp_dir, "state1.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                s1 = json.load(f)
            assert s1["buttons"]["a"] is True

            session.send_command(f"INJECT {json.dumps({'a': False})}")
            session.send_command("TICK")
            
            st_path2 = os.path.join(self.temp_dir, "state2.json")
            session.send_command(f"DUMP_STATE {st_path2}")
            with open(st_path2, "r") as f:
                s2 = json.load(f)
            assert s2["buttons"]["a"] is False
        finally:
            session.close()

    # --- Feature 5: Speed Control (T5.1 - T5.5) ---

    def test_t5_1_half_speed(self) -> None:
        """T5.1: Half Speed (0.5x)."""
        result = self.runner.run(ticks=1, speed=0.5)
        assert result.is_success
        state = result.parse_state()
        assert state["speed"] == 0.5
        assert state["cpu_cycles"] == int(70224 * 0.5)
        assert len(result.read_audio_buffer()) == int(735 * 0.5) * 4

    def test_t5_2_normal_speed(self) -> None:
        """T5.2: Normal Speed (1.0x)."""
        result = self.runner.run(ticks=1, speed=1.0)
        assert result.is_success
        state = result.parse_state()
        assert state["speed"] == 1.0
        assert state["cpu_cycles"] == 70224
        assert len(result.read_audio_buffer()) == 735 * 4

    def test_t5_3_double_speed(self) -> None:
        """T5.3: Double Speed (2.0x)."""
        result = self.runner.run(ticks=1, speed=2.0)
        assert result.is_success
        state = result.parse_state()
        assert state["speed"] == 2.0
        assert state["cpu_cycles"] == 70224 * 2
        assert len(result.read_audio_buffer()) == 735 * 2 * 4

    def test_t5_4_quad_speed(self) -> None:
        """T5.4: Quad Speed (4.0x)."""
        result = self.runner.run(ticks=1, speed=4.0)
        assert result.is_success
        state = result.parse_state()
        assert state["speed"] == 4.0
        assert state["cpu_cycles"] == 70224 * 4
        assert len(result.read_audio_buffer()) == 735 * 4 * 4

    def test_t5_5_invalid_speed_limits(self) -> None:
        """T5.5: Invalid Speed Limits."""
        session = self.spawn_interactive()
        try:
            assert "SET_SPEED_ERROR" in session.send_command("SET_SPEED -1")
            assert "SET_SPEED_ERROR" in session.send_command("SET_SPEED 0")
            assert "SET_SPEED_ERROR" in session.send_command("SET_SPEED 2000")
        finally:
            session.close()

    # --- Feature 6: Frame Skipping (T6.1 - T6.5) ---

    def test_t6_1_frame_skip_0(self) -> None:
        """T6.1: Frame Skip 0."""
        result = self.runner.run(ticks=3, frame_skip=0)
        assert result.is_success
        state = result.parse_state()
        assert state["frame_skip"] == 0
        assert state["rendered_frames"] == 3

    def test_t6_2_frame_skip_1(self) -> None:
        """T6.2: Frame Skip 1 (rendered_frames is half)."""
        result = self.runner.run(ticks=4, frame_skip=1)
        assert result.is_success
        state = result.parse_state()
        assert state["frame_skip"] == 1
        # Ticks are 1, 2, 3, 4. Renders on ticks 2, 4 (ticks % 2 == 0) -> 2 rendered frames
        assert state["rendered_frames"] == 2

    def test_t6_3_frame_skip_3(self) -> None:
        """T6.3: Frame Skip 3."""
        result = self.runner.run(ticks=8, frame_skip=3)
        assert result.is_success
        state = result.parse_state()
        assert state["frame_skip"] == 3
        # Renders on ticks 4, 8 -> 2 rendered frames
        assert state["rendered_frames"] == 2

    def test_t6_4_frame_skip_paused(self) -> None:
        """T6.4: Frame Skip Paused (no frames rendered)."""
        result = self.runner.run(ticks=5, frame_skip=2, extra_args=["--pause"])
        assert result.is_success
        state = result.parse_state()
        assert state["rendered_frames"] == 0

    def test_t6_5_dynamic_skip_change(self) -> None:
        """T6.5: Dynamic Skip Change."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            assert session.send_command("SET_FRAME_SKIP 1") == "SET_FRAME_SKIP_OK"
            for _ in range(4):
                session.send_command("TICK")
            
            st_path = os.path.join(self.temp_dir, "state1.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                s1 = json.load(f)
            assert s1["rendered_frames"] == 2

            assert session.send_command("SET_FRAME_SKIP 0") == "SET_FRAME_SKIP_OK"
            for _ in range(4):
                session.send_command("TICK")
            
            st_path2 = os.path.join(self.temp_dir, "state2.json")
            session.send_command(f"DUMP_STATE {st_path2}")
            with open(st_path2, "r") as f:
                s2 = json.load(f)
            # Renders: 2 (from skip 1) + 4 (from skip 0) = 6
            assert s2["rendered_frames"] == 6
        finally:
            session.close()

    # --- Feature 7: Atomic Savestates (T7.1 - T7.5) ---

    def test_t7_1_atomic_write(self) -> None:
        """T7.1: Atomic Write (creates .sav file)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command("TICK")
            assert session.send_command("SAVE_STATE 0") == "SAVE_STATE_OK"
            sav_file = os.path.join(self.temp_dir, "savestate_0.sav")
            assert os.path.exists(sav_file)
        finally:
            session.close()

    def test_t7_2_state_restore(self) -> None:
        """T7.2: State Restore."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            # Go to gameplay
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")

            # Save state at (80, 72)
            session.send_command("SAVE_STATE 1")

            # Move character to (85, 72)
            for _ in range(5):
                session.send_command(f"INJECT {json.dumps({'right': True})}")
                session.send_command("TICK")

            st_moved = os.path.join(self.temp_dir, "moved.json")
            session.send_command(f"DUMP_STATE {st_moved}")
            with open(st_moved, "r") as f:
                s_moved = json.load(f)
            assert s_moved["player_x"] == 85

            # Restore state
            assert session.send_command("LOAD_STATE 1") == "LOAD_STATE_OK"
            st_restored = os.path.join(self.temp_dir, "restored.json")
            session.send_command(f"DUMP_STATE {st_restored}")
            with open(st_restored, "r") as f:
                s_rest = json.load(f)
            assert s_rest["player_x"] == 80
        finally:
            session.close()

    def test_t7_3_bad_save_file_rejection(self) -> None:
        """T7.3: Bad Save File Rejection."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command("TICK")
            # Write a corrupt savestate file (missing keys)
            sav_path = os.path.join(self.temp_dir, "savestate_2.sav")
            with open(sav_path, "w") as f:
                f.write('{"console_type": "GBC"}')
            
            resp = session.send_command("LOAD_STATE 2")
            assert "LOAD_STATE_ERROR" in resp
        finally:
            session.close()

    def test_t7_4_multi_slot_savestates(self) -> None:
        """T7.4: Multi-Slot Savestates."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            # Go to gameplay
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")

            # Save slot 0 at (80, 72)
            session.send_command("SAVE_STATE 0")

            # Move character to (85, 72)
            for _ in range(5):
                session.send_command(f"INJECT {json.dumps({'right': True})}")
                session.send_command("TICK")
            # Save slot 1 at (85, 72)
            session.send_command("SAVE_STATE 1")

            # Load slot 0
            session.send_command("LOAD_STATE 0")
            st_p = os.path.join(self.temp_dir, "state0.json")
            session.send_command(f"DUMP_STATE {st_p}")
            with open(st_p, "r") as f:
                s0 = json.load(f)
            assert s0["player_x"] == 80

            # Load slot 1
            session.send_command("LOAD_STATE 1")
            st_p2 = os.path.join(self.temp_dir, "state1.json")
            session.send_command(f"DUMP_STATE {st_p2}")
            with open(st_p2, "r") as f:
                s1 = json.load(f)
            assert s1["player_x"] == 85
        finally:
            session.close()

    def test_t7_5_rename_failure_robustness(self) -> None:
        """T7.5: Rename Failure Robustness."""
        # Clean savestate file
        sav_path = os.path.join(self.temp_dir, "savestate_0.sav")
        with open(sav_path, "w") as f:
            f.write('{"state": "existing"}')

        os.environ["MOCK_DISK_FULL"] = "1"
        try:
            session = self.spawn_interactive()
            try:
                resp = session.send_command("SAVE_STATE 0")
                assert "SAVE_STATE_ERROR" in resp
                
                # The existing savestate must NOT be corrupted/deleted
                with open(sav_path, "r") as f:
                    content = f.read()
                assert "existing" in content
            finally:
                session.close()
        finally:
            if "MOCK_DISK_FULL" in os.environ:
                del os.environ["MOCK_DISK_FULL"]

    # --- Feature 8: CPU/APU Execution Mocks (T8.1 - T8.5) ---

    def test_t8_1_gbc_cpu_ticking(self) -> None:
        """T8.1: GBC CPU Ticking."""
        result = self.runner.run(ticks=3)
        assert result.is_success
        state = result.parse_state()
        assert state["cpu_cycles"] == 70224 * 3

    def test_t8_2_gba_cpu_ticking(self) -> None:
        """T8.2: GBA CPU Ticking."""
        rom_path = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", rom_path)
        result = self.runner.run(ticks=3, rom=rom_path)
        assert result.is_success
        state = result.parse_state()
        assert state["cpu_cycles"] == 280896 * 3

    def test_t8_3_audio_stereo_footprint(self) -> None:
        """T8.3: Audio Stereo Footprint (stereo 16-bit PCM: 4 bytes/sample)."""
        result = self.runner.run(ticks=5)
        assert result.is_success
        audio = result.read_audio_buffer()
        # 5 frames * 735 samples * 4 bytes = 14700 bytes
        assert len(audio) == 14700

    def test_t8_4_audio_mute_volume(self) -> None:
        """T8.4: Audio Mute/Volume (zero filled when paused)."""
        result = self.runner.run(ticks=5, extra_args=["--pause"])
        assert result.is_success
        audio = result.read_audio_buffer()
        for b in audio:
            assert b == 0

    def test_t8_5_cpu_apu_cycle_sync(self) -> None:
        """T8.5: CPU-APU Cycle Sync."""
        # At 2.0x speed, both cycles and audio output sizes double
        result = self.runner.run(ticks=1, speed=2.0)
        assert result.is_success
        state = result.parse_state()
        audio = result.read_audio_buffer()
        assert state["cpu_cycles"] == 70224 * 2
        assert len(audio) == 735 * 2 * 4


# ==============================================================================
# TIER 2: Boundary & Corner Cases (40 cases)
# ==============================================================================

class TestTier2BoundaryCases(TestBase):
    """Tier 2: Boundary & Corner Cases (B1.1 - B8.5)."""

    # --- Feature 1: ROM Scanning & Path Safety ---

    def test_b1_1_scan_empty_directory(self) -> None:
        """B1.1: Scan empty ROM directory."""
        empty_dir = os.path.join(self.temp_dir, "empty")
        os.makedirs(empty_dir, exist_ok=True)
        session = self.spawn_interactive()
        try:
            resp = session.send_command(f"SCAN_ROMS {empty_dir}")
            assert resp.startswith("SCAN_ROMS_OK")
            data = json.loads(resp.split(maxsplit=1)[1])
            assert len(data) == 0
        finally:
            session.close()

    def test_b1_2_scan_directory_with_1000_files(self) -> None:
        """B1.2: Scan directory with 1000+ files."""
        many_dir = os.path.join(self.temp_dir, "many")
        os.makedirs(many_dir, exist_ok=True)
        for i in range(1000):
            with open(os.path.join(many_dir, f"file_{i}.txt"), "w") as f:
                f.write("text")
        create_mock_rom("GBC", os.path.join(many_dir, "real.gbc"))

        session = self.spawn_interactive()
        try:
            resp = session.send_command(f"SCAN_ROMS {many_dir}")
            assert resp.startswith("SCAN_ROMS_OK")
            data = json.loads(resp.split(maxsplit=1)[1])
            assert len(data) == 1
            assert os.path.basename(data[0]["path"]) == "real.gbc"
        finally:
            session.close()

    def test_b1_3_rom_paths_with_spaces(self) -> None:
        """B1.3: ROM paths containing special characters / spaces."""
        rom_path = os.path.join(self.temp_dir, "game with spaces & chars!@#.gbc")
        create_mock_rom("GBC", rom_path)
        session = self.spawn_interactive()
        try:
            assert session.send_command(f"LOAD_ROM {rom_path}") == "LOAD_ROM_OK"
        finally:
            session.close()

    def test_b1_4_deep_nested_traversals(self) -> None:
        """B1.4: Relative paths starting with deep nested ../../ back-traversals."""
        session = self.spawn_interactive()
        try:
            resp = session.send_command("LOAD_ROM ../../../../../../../etc/hosts")
            assert "LOAD_ROM_ERROR Path traversal detected" in resp
        finally:
            session.close()

    def test_b1_5_extremely_long_path(self) -> None:
        """B1.5: Extremely long file path."""
        long_dir = os.path.join(self.temp_dir, "a" * 150)
        os.makedirs(long_dir, exist_ok=True)
        rom_path = os.path.join(long_dir, "b" * 50 + ".gbc")
        create_mock_rom("GBC", rom_path)
        session = self.spawn_interactive()
        try:
            assert session.send_command(f"LOAD_ROM {rom_path}") == "LOAD_ROM_OK"
        finally:
            session.close()

    # --- Feature 2: Header Parsing ---

    def test_b2_1_header_min_size_rom(self) -> None:
        """B2.1: Header validation on minimum size ROM."""
        rom_path = os.path.join(self.temp_dir, "min.gbc")
        create_mock_rom("GBC", rom_path)
        session = self.spawn_interactive()
        try:
            assert session.send_command(f"LOAD_ROM {rom_path}") == "LOAD_ROM_OK"
        finally:
            session.close()

    def test_b2_2_checksum_off_by_one(self) -> None:
        """B2.2: Complement checksum off-by-one error."""
        rom_path = os.path.join(self.temp_dir, "bad_checksum_1.gbc")
        create_mock_rom("GBC", rom_path, corrupt_checksum=True)
        session = self.spawn_interactive()
        try:
            resp = session.send_command(f"LOAD_ROM {rom_path}")
            assert "LOAD_ROM_ERROR GBC header checksum mismatch" in resp
        finally:
            session.close()

    def test_b2_3_truncated_header(self) -> None:
        """B2.3: Parse ROM with truncated header."""
        rom_path = os.path.join(self.temp_dir, "trunc.gba")
        create_mock_rom("GBA", rom_path, truncated=True)
        session = self.spawn_interactive()
        try:
            resp = session.send_command(f"LOAD_ROM {rom_path}")
            assert "LOAD_ROM_ERROR Truncated ROM" in resp
        finally:
            session.close()

    def test_b2_4_non_ascii_game_title(self) -> None:
        """B2.4: Parse ROM with non-ASCII characters in game title."""
        rom_path = os.path.join(self.temp_dir, "ascii.gbc")
        create_mock_rom("GBC", rom_path, non_ascii_title=True)
        session = self.spawn_interactive()
        try:
            assert session.send_command(f"LOAD_ROM {rom_path}") == "LOAD_ROM_OK"
        finally:
            session.close()

    def test_b2_5_corrupted_payload(self) -> None:
        """B2.5: Valid header but corrupted ROM payload."""
        rom_path = os.path.join(self.temp_dir, "corrupt_payload.gbc")
        create_mock_rom("GBC", rom_path)
        with open(rom_path, "ab") as f:
            f.write(b"garbage" * 100)
        session = self.spawn_interactive()
        try:
            assert session.send_command(f"LOAD_ROM {rom_path}") == "LOAD_ROM_OK"
        finally:
            session.close()

    # --- Feature 3: Dynamic GBC/GBA Resolution ---

    def test_b3_1_screen_dimension_bounds(self) -> None:
        """B3.1: Screen dimension bounds checking."""
        rom_gbc = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", rom_gbc)
        session = self.spawn_interactive()
        try:
            session.send_command(f"LOAD_ROM {rom_gbc}")
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["console_type"] == "GBC"
        finally:
            session.close()

    def test_b3_2_zero_copy_alignment(self) -> None:
        """B3.2: Verify zero-copy buffer pointer alignment (16-byte aligned)."""
        session = self.spawn_interactive()
        try:
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["video_buffer_addr"] % 16 == 0
            assert state["audio_buffer_addr"] % 16 == 0
        finally:
            session.close()

    def test_b3_3_rapid_switch_stress(self) -> None:
        """B3.3: Rapid resolution switching stress test."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBC", gbc_rom)
        create_mock_rom("GBA", gba_rom)

        session = self.spawn_interactive()
        try:
            for _ in range(25):
                session.send_command(f"LOAD_ROM {gbc_rom}")
                session.send_command(f"LOAD_ROM {gba_rom}")
            resp = session.send_command("PLAY")
            assert resp == "PLAY_OK"
        finally:
            session.close()

    def test_b3_4_buffer_access_during_switch(self) -> None:
        """B3.4: Video buffer access during resolution switch."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", gbc_rom)
        session = self.spawn_interactive()
        try:
            session.send_command(f"LOAD_ROM {gbc_rom}")
            video_p = os.path.join(self.temp_dir, "vid.raw")
            assert session.send_command(f"DUMP_VIDEO {video_p}") == "DUMP_VIDEO_OK"
            assert len(open(video_p, "rb").read()) == 69120
        finally:
            session.close()

    def test_b3_5_max_buffer_index_bounds(self) -> None:
        """B3.5: Max possible video buffer index out-of-bounds safety."""
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", gba_rom)
        session = self.spawn_interactive()
        try:
            session.send_command(f"LOAD_ROM {gba_rom}")
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'right': True})}")
            for _ in range(250):
                session.send_command("TICK")
            video_p = os.path.join(self.temp_dir, "vid.raw")
            session.send_command(f"DUMP_VIDEO {video_p}")
            assert len(open(video_p, "rb").read()) == 115200
        finally:
            session.close()

    # --- Feature 4: Controller Button Mappings ---

    def test_b4_1_extreme_mash(self) -> None:
        """B4.1: Mash all inputs including L/R at 60Hz."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            all_btn = {"up": True, "down": True, "left": True, "right": True, "a": True, "b": True, "start": True, "select": True, "l": True, "r": True}
            for _ in range(30):
                session.send_command(f"INJECT {json.dumps(all_btn)}")
                session.send_command("TICK")
            assert "TICK_OK" in session.send_command("TICK")
        finally:
            session.close()

    def test_b4_2_invalid_buttons_json(self) -> None:
        """B4.2: Input injection with invalid buttons JSON format."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            resp = session.send_command('INJECT {"unknown_button": true}')
            assert resp == "INJECT_OK"
        finally:
            session.close()

    def test_b4_3_socd_neutralization_high_speed(self) -> None:
        """B4.3: SOCD neutralization at extreme high speed."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")

            for _ in range(20):
                session.send_command(f"INJECT {json.dumps({'left': True, 'right': True})}")
                session.send_command("TICK")
                
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["player_x"] == 80
        finally:
            session.close()

    def test_b4_4_inject_during_loading(self) -> None:
        """B4.4: Injecting inputs while emulator is in transition/loading states."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", gbc_rom)
        session = self.spawn_interactive()
        try:
            session.send_command(f"INJECT {json.dumps({'a': True})}")
            session.send_command(f"LOAD_ROM {gbc_rom}")
            assert "TICK_OK" in session.send_command("TICK")
        finally:
            session.close()

    def test_b4_5_redundant_releases(self) -> None:
        """B4.5: Redundant releases of buttons."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'a': False})}")
            session.send_command("TICK")
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["buttons"]["a"] is False
        finally:
            session.close()

    # --- Feature 5: Speed Control ---

    def test_b5_1_min_speed(self) -> None:
        """B5.1: Set speed to extremely small positive float (e.g. 1e-6)."""
        session = self.spawn_interactive()
        try:
            assert session.send_command("SET_SPEED 0.000001") == "SET_SPEED_OK"
        finally:
            session.close()

    def test_b5_2_max_speed(self) -> None:
        """B5.2: Set speed to very high float (e.g. 1000.0)."""
        session = self.spawn_interactive()
        try:
            assert session.send_command("SET_SPEED 1000.0") == "SET_SPEED_OK"
        finally:
            session.close()

    def test_b5_3_speed_change_paused(self) -> None:
        """B5.3: Speed change on paused emulator."""
        session = self.spawn_interactive()
        try:
            session.send_command("PAUSE")
            assert session.send_command("SET_SPEED 2.0") == "SET_SPEED_OK"
            session.send_command("TICK")
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["ticks"] == 0
        finally:
            session.close()

    def test_b5_4_rapid_speed_changes(self) -> None:
        """B5.4: Rapid alternating speed changes on every tick."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            for i in range(10):
                spd = 0.5 if i % 2 == 0 else 4.0
                session.send_command(f"SET_SPEED {spd}")
                session.send_command("TICK")
            assert "TICK_OK" in session.send_command("TICK")
        finally:
            session.close()

    def test_b5_5_non_numeric_speed(self) -> None:
        """B5.5: Non-numeric speed value error handling."""
        session = self.spawn_interactive()
        try:
            resp = session.send_command("SET_SPEED fast")
            assert "SET_SPEED_ERROR Non-numeric speed" in resp
        finally:
            session.close()

    # --- Feature 6: Frame Skipping ---

    def test_b6_1_negative_frame_skip(self) -> None:
        """B6.1: Frame skip set to negative values."""
        session = self.spawn_interactive()
        try:
            resp = session.send_command("SET_FRAME_SKIP -5")
            assert "SET_FRAME_SKIP_ERROR" in resp
        finally:
            session.close()

    def test_b6_2_large_frame_skip(self) -> None:
        """B6.2: Frame skip set to extremely large count (e.g. 1000)."""
        session = self.spawn_interactive()
        try:
            assert session.send_command("SET_FRAME_SKIP 1000") == "SET_FRAME_SKIP_OK"
        finally:
            session.close()

    def test_b6_3_skip_change_while_ticking(self) -> None:
        """B6.3: Frame skip change while actively ticking."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command("SET_FRAME_SKIP 2")
            session.send_command("TICK")
            session.send_command("SET_FRAME_SKIP 0")
            session.send_command("TICK")
            assert "TICK_OK" in session.send_command("TICK")
        finally:
            session.close()

    def test_b6_4_skip_buffer_stability(self) -> None:
        """B6.4: Frame skip video buffer stability checks."""
        result = self.runner.run(ticks=3, frame_skip=2)
        assert result.is_success
        video = result.read_video_buffer()
        assert len(video) == 69120

    def test_b6_5_skip_overhead(self) -> None:
        """B6.5: Verify no frame skip lag (catch-up overhead)."""
        start = time.time()
        result = self.runner.run(ticks=100, frame_skip=10)
        assert result.is_success
        assert (time.time() - start) < 1.0

    # --- Feature 7: Atomic Savestates ---

    def test_b7_1_simulated_full_disk_save(self) -> None:
        """B7.1: Save state on simulated full disk."""
        os.environ["MOCK_DISK_FULL"] = "1"
        try:
            session = self.spawn_interactive()
            try:
                resp = session.send_command("SAVE_STATE 0")
                assert "SAVE_STATE_ERROR" in resp
            finally:
                session.close()
        finally:
            if "MOCK_DISK_FULL" in os.environ:
                del os.environ["MOCK_DISK_FULL"]

    def test_b7_2_load_non_existent_slot(self) -> None:
        """B7.2: Load savestate from empty/non-existent slot."""
        session = self.spawn_interactive()
        try:
            resp = session.send_command("LOAD_STATE 99")
            assert "LOAD_STATE_ERROR File not found" in resp
        finally:
            session.close()

    def test_b7_3_load_corrupt_version(self) -> None:
        """B7.3: Load savestate from a file with wrong version/hash."""
        sav_path = os.path.join(self.temp_dir, "savestate_0.sav")
        with open(sav_path, "w") as f:
            f.write("corrupted data string")
        session = self.spawn_interactive()
        try:
            resp = session.send_command("LOAD_STATE 0")
            assert "LOAD_STATE_ERROR" in resp
        finally:
            session.close()

    def test_b7_4_rapid_saves_loads(self) -> None:
        """B7.4: Rapid consecutive saves and loads."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command("TICK")
            for _ in range(10):
                session.send_command("SAVE_STATE 0")
                session.send_command("LOAD_STATE 0")
            assert "TICK_OK" in session.send_command("TICK")
        finally:
            session.close()

    def test_b7_5_save_invalid_directory(self) -> None:
        """B7.5: Save state to an invalid directory path."""
        session = self.spawn_interactive()
        try:
            resp = session.send_command("SAVE_STATE ../../outside")
            assert "SAVE_STATE_ERROR Path traversal detected" in resp
        finally:
            session.close()

    # --- Feature 8: CPU/APU Execution Mocks ---

    def test_b8_1_cycle_overflow(self) -> None:
        """B8.1: Run CPU cycle count that overflows standard uint64 (rolls over)."""
        sav_path = os.path.join(self.temp_dir, "savestate_0.sav")
        state_data = {
            "console_type": "GBC",
            "playback_state": "play",
            "ticks": 0,
            "player_x": 80,
            "player_y": 72,
            "buttons": {btn: False for btn in ["up", "down", "left", "right", "a", "b", "start", "select", "l", "r"]},
            "speed": 1.0,
            "frame_skip": 0,
            "cpu_cycles": 0xFFFFFFFFFFFFFFF0,
            "rendered_frames": 0,
        }
        with open(sav_path, "w") as f:
            json.dump(state_data, f)
            
        session = self.spawn_interactive()
        try:
            session.send_command("LOAD_STATE 0")
            session.send_command("PLAY")
            session.send_command("TICK")
            
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["cpu_cycles"] < 70224
        finally:
            session.close()

    def test_b8_2_cpu_halting(self) -> None:
        """B8.2: CPU execution halting instruction behavior (pause state)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command("TICK")
            session.send_command("PAUSE")
            session.send_command("TICK")
            
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["cpu_cycles"] == 70224
        finally:
            session.close()

    def test_b8_3_apu_underflow(self) -> None:
        """B8.3: APU audio buffer underflow simulation."""
        result = self.runner.run(ticks=10)
        assert result.is_success
        assert len(result.read_audio_buffer()) > 0

    def test_b8_4_apu_overflow(self) -> None:
        """B8.4: APU audio buffer overflow simulation."""
        result = self.runner.run(ticks=500)
        assert result.is_success
        assert len(result.read_audio_buffer()) == 500 * 735 * 4

    def test_b8_5_apu_resampler_edge(self) -> None:
        """B8.5: APU frequency resampler edge case (extremely low speed multiplier)."""
        result = self.runner.run(ticks=1, speed=0.001)
        assert result.is_success
        state = result.parse_state()
        assert state["speed"] == 0.001


# ==============================================================================
# TIER 3: Cross-Feature Combinations (8 cases)
# ==============================================================================

class TestTier3CrossFeature(TestBase):
    """Tier 3: Cross-Feature Combinations (C3.1 - C3.8)."""

    def test_c3_1_rom_load_resolution_speed(self) -> None:
        """C3.1: ROM Loading (GBA) + Resolution Switch + Speed Control (GBA ROM at 2x speed)."""
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", gba_rom)
        result = self.runner.run(ticks=1, rom=gba_rom, speed=2.0)
        assert result.is_success
        state = result.parse_state()
        assert state["console_type"] == "GBA"
        assert state["speed"] == 2.0
        assert len(result.read_video_buffer()) == 115200
        assert state["cpu_cycles"] == int(280896 * 2.0)

    def test_c3_2_savestate_speed(self) -> None:
        """C3.2: Savestate Save + Speed Control (save state at 4x speed, verify reload)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command("SET_SPEED 4.0")
            session.send_command("SAVE_STATE 0")
            session.send_command("SET_SPEED 1.0")
            session.send_command("LOAD_STATE 0")
            
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["speed"] == 4.0
        finally:
            session.close()

    def test_c3_3_frame_skip_speed(self) -> None:
        """C3.3: Frame Skipping + Speed Control (GBC at 2x speed with frame skip 2)."""
        result = self.runner.run(ticks=6, speed=2.0, frame_skip=2)
        assert result.is_success
        state = result.parse_state()
        assert state["speed"] == 2.0
        assert state["frame_skip"] == 2
        assert state["rendered_frames"] == 2

    def test_c3_4_input_frame_skip(self) -> None:
        """C3.4: Controller Input (L/R) + Frame Skipping (inject inputs during skipped frames)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command("SET_FRAME_SKIP 2")
            session.send_command(f"INJECT {json.dumps({'l': True, 'r': True})}")
            session.send_command("TICK")
            
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["buttons"]["l"] is True
            assert state["buttons"]["r"] is True
        finally:
            session.close()

    def test_c3_5_rom_load_reset_savestate(self) -> None:
        """C3.5: ROM Load (GBC) + Reset + Savestate Load."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", gbc_rom)
        
        session = self.spawn_interactive()
        try:
            session.send_command(f"LOAD_ROM {gbc_rom}")
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")
            
            session.send_command("SAVE_STATE 0")
            session.send_command("RESET")
            
            session.send_command("LOAD_STATE 0")
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["console_type"] == "GBC"
            assert state["ticks"] == 2
        finally:
            session.close()

    def test_c3_6_savestate_resolution(self) -> None:
        """C3.6: Savestate Save + Resolution Switch."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBC", gbc_rom)
        create_mock_rom("GBA", gba_rom)

        session = self.spawn_interactive()
        try:
            session.send_command(f"LOAD_ROM {gbc_rom}")
            session.send_command("SAVE_STATE 0")
            session.send_command(f"LOAD_ROM {gba_rom}")
            
            st_path1 = os.path.join(self.temp_dir, "state1.json")
            session.send_command(f"DUMP_STATE {st_path1}")
            with open(st_path1, "r") as f:
                s1 = json.load(f)
            assert s1["console_type"] == "GBA"

            session.send_command("LOAD_STATE 0")
            st_path2 = os.path.join(self.temp_dir, "state2.json")
            session.send_command(f"DUMP_STATE {st_path2}")
            with open(st_path2, "r") as f:
                s2 = json.load(f)
            assert s2["console_type"] == "GBC"
        finally:
            session.close()

    def test_c3_7_cycle_exhaustion_audio_sync(self) -> None:
        """C3.7: CPU cycle exhaustion + APU synchronization under speed."""
        result = self.runner.run(ticks=5, speed=4.0)
        assert result.is_success
        state = result.parse_state()
        audio = result.read_audio_buffer()
        assert state["cpu_cycles"] == 70224 * 4 * 5
        assert len(audio) == 735 * 4 * 5 * 4

    def test_c3_8_savestate_slot_path_traversal(self) -> None:
        """C3.8: Path traversal injection via savestate filename slot parameters."""
        session = self.spawn_interactive()
        try:
            resp = session.send_command("SAVE_STATE ../../outside_slot")
            assert "SAVE_STATE_ERROR Path traversal detected" in resp
        finally:
            session.close()


# ==============================================================================
# TIER 4: Real-World Walkthroughs/Performance (5 cases)
# ==============================================================================

class TestTier4GameplayWalkthroughs(TestBase):
    """Tier 4: Real-World Gameplay Walkthroughs & Headless Performance."""

    def test_w4_1_gbc_walkthrough(self) -> None:
        """W4.1: GBC Walkthrough."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", gbc_rom)

        session = self.spawn_interactive()
        try:
            assert session.send_command(f"LOAD_ROM {gbc_rom}") == "LOAD_ROM_OK"
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")
            
            for _ in range(5):
                session.send_command(f"INJECT {json.dumps({'right': True})}")
                session.send_command("TICK")
            
            st_path1 = os.path.join(self.temp_dir, "state1.json")
            session.send_command(f"DUMP_STATE {st_path1}")
            with open(st_path1, "r") as f:
                s1 = json.load(f)
            assert s1["player_x"] == 85

            session.send_command("SAVE_STATE 0")

            for _ in range(5):
                session.send_command(f"INJECT {json.dumps({'right': True})}")
                session.send_command("TICK")
                
            st_path2 = os.path.join(self.temp_dir, "state2.json")
            session.send_command(f"DUMP_STATE {st_path2}")
            with open(st_path2, "r") as f:
                s2 = json.load(f)
            assert s2["player_x"] == 90

            session.send_command("LOAD_STATE 0")
            st_path3 = os.path.join(self.temp_dir, "state3.json")
            session.send_command(f"DUMP_STATE {st_path3}")
            with open(st_path3, "r") as f:
                s3 = json.load(f)
            assert s3["player_x"] == 85
        finally:
            session.close()

    def test_w4_2_gba_playthrough_lr(self) -> None:
        """W4.2: GBA Playthrough with L/R triggers."""
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", gba_rom)

        session = self.spawn_interactive()
        try:
            assert session.send_command(f"LOAD_ROM {gba_rom}") == "LOAD_ROM_OK"
            session.send_command("PLAY")
            
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")

            session.send_command(f"INJECT {json.dumps({'l': True, 'r': True, 'right': True})}")
            session.send_command("TICK")

            st_path1 = os.path.join(self.temp_dir, "state1.json")
            session.send_command(f"DUMP_STATE {st_path1}")
            with open(st_path1, "r") as f:
                s1 = json.load(f)
            assert s1["console_type"] == "GBA"
            assert s1["buttons"]["l"] is True
            assert s1["buttons"]["r"] is True
            assert s1["player_x"] == 121
        finally:
            session.close()

    def test_w4_3_fast_forward_skip(self) -> None:
        """W4.3: Fast-Forward and Skip Mode."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", gbc_rom)

        result = self.runner.run(ticks=240, rom=gbc_rom, speed=4.0, frame_skip=3)
        assert result.is_success
        state = result.parse_state()
        audio = result.read_audio_buffer()
        
        assert len(audio) == 240 * int(735 * 4.0) * 4
        assert state["rendered_frames"] == 60

    def test_w4_4_multi_rom_session(self) -> None:
        """W4.4: Multi-ROM Session Switcher."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBC", gbc_rom)
        create_mock_rom("GBA", gba_rom)

        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command(f"LOAD_ROM {gbc_rom}")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command("SAVE_STATE 0")

            session.send_command(f"LOAD_ROM {gba_rom}")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command("SAVE_STATE 1")

            session.send_command(f"LOAD_ROM {gbc_rom}")
            session.send_command("LOAD_STATE 0")
            
            st_path = os.path.join(self.temp_dir, "state.json")
            session.send_command(f"DUMP_STATE {st_path}")
            with open(st_path, "r") as f:
                state = json.load(f)
            assert state["console_type"] == "GBC"
        finally:
            session.close()

    def test_w4_5_high_performance_benchmark(self) -> None:
        """W4.5: High-Performance Benchmark."""
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", gba_rom)

        start = time.time()
        result = self.runner.run(ticks=10000, rom=gba_rom, dump_video=False, dump_audio=False)
        assert result.is_success
        elapsed = time.time() - start
        avg_tick_ms = (elapsed / 10000.0) * 1000.0
        assert avg_tick_ms < 1.0
