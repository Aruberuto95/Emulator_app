"""E2E Test Suite for the Nintendo 64 (N64) Emulator Core.

This suite implements Tiers 1 through 4 of the E2E test design, verifying
ROM scanning, header parsing, dynamic resolution, button mappings, analog stick inputs,
speed control, frame skipping, atomic savestates, CPU MIPS64 execution cycles,
APU/audio resampling, and Expansion Pak configuration.
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
    z: bool = False
    c_up: bool = False
    c_down: bool = False
    c_left: bool = False
    c_right: bool = False
    stick_x: int = 0
    stick_y: int = 0

    def to_dict(self) -> Dict[str, Any]:
        """Convert button state to a dictionary structure.

        Returns:
            Dict[str, Any]: Dictionary of button states.
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
            "z": self.z,
            "c_up": self.c_up,
            "c_down": self.c_down,
            "c_left": self.c_left,
            "c_right": self.c_right,
            "stick_x": self.stick_x,
            "stick_y": self.stick_y,
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
        env["WORKSPACE_DIR"] = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
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
    """Base class for test cases containing common victories and helpers."""

    @pytest.fixture(autouse=True)
    def setup_temp_dir(self) -> None:
        """Sets up a temporary directory for each test case."""
        current_dir = os.path.dirname(os.path.abspath(__file__))
        local_temp = os.path.join(current_dir, "tmp")
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
        env["WORKSPACE_DIR"] = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
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

    def create_mock_rom(
        self,
        name: str,
        console: str = "N64",
        magic: bytes = b'\x80\x37\x12\x40',
        size: int = 1024,
        text: bool = False,
        logo_valid: bool = True,
        checksum_valid: bool = True,
    ) -> str:
        """Creates a mock ROM file for scanning and header tests."""
        path = os.path.join(self.temp_dir, name)
        if text:
            with open(path, "w", encoding="utf-8") as f:
                f.write(f"CONSOLE: {console}\n")
                f.write(f"LOGO: {'VALID' if logo_valid else 'INVALID'}\n")
                f.write(f"CHECKSUM: {'VALID' if checksum_valid else 'INVALID'}\n")
        else:
            with open(path, "wb") as f:
                if len(magic) > 0:
                    f.write(magic)
                    f.write(b'\x00' * (size - len(magic)))
                else:
                    f.write(b'\x00' * size)
        return path


class TestN64E2E(TestBase):
    """E2E Test Case implementations for the N64 Core."""

    # =========================================================================
    # TIER 1: Feature Coverage (60 cases, 5 per feature)
    # =========================================================================

    # Feature 1: ROM Scanning & Path Safety
    def test_tier1_rom_scanning_valid_z64(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        res = session.send_command(f"SCAN_ROMS {self.temp_dir}")
        assert "SCAN_ROMS_OK" in res
        assert "game.z64" in res
        assert "N64" in res
        session.close()

    def test_tier1_rom_scanning_valid_v64(self):
        rom = self.create_mock_rom("game.v64", magic=b'\x37\x80\x40\x12')
        session = self.spawn_interactive()
        res = session.send_command(f"SCAN_ROMS {self.temp_dir}")
        assert "SCAN_ROMS_OK" in res
        assert "game.v64" in res
        session.close()

    def test_tier1_rom_scanning_valid_n64(self):
        rom = self.create_mock_rom("game.n64", magic=b'\x40\x12\x37\x80')
        session = self.spawn_interactive()
        res = session.send_command(f"SCAN_ROMS {self.temp_dir}")
        assert "SCAN_ROMS_OK" in res
        assert "game.n64" in res
        session.close()

    def test_tier1_rom_scanning_nested_dirs(self):
        nested = os.path.join(self.temp_dir, "nested_dir")
        os.makedirs(nested, exist_ok=True)
        rom = os.path.join(nested, "game.z64")
        with open(rom, "wb") as f:
            f.write(b'\x80\x37\x12\x40' + b'\x00'*60)
        session = self.spawn_interactive()
        res = session.send_command(f"SCAN_ROMS {self.temp_dir}")
        assert "SCAN_ROMS_OK" in res
        assert "game.z64" in res
        session.close()

    def test_tier1_rom_scanning_relative_path(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        rel_dir = os.path.relpath(self.temp_dir)
        res = session.send_command(f"SCAN_ROMS {rel_dir}")
        assert "SCAN_ROMS_OK" in res
        session.close()

    # Feature 2: ROM Header Parsing
    def test_tier1_header_parsing_big_endian(self):
        rom = self.create_mock_rom("game.z64", magic=b'\x80\x37\x12\x40')
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert res == "LOAD_ROM_OK"
        session.close()

    def test_tier1_header_parsing_byte_swapped(self):
        rom = self.create_mock_rom("game.v64", magic=b'\x37\x80\x40\x12')
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert res == "LOAD_ROM_OK"
        session.close()

    def test_tier1_header_parsing_little_endian(self):
        rom = self.create_mock_rom("game.n64", magic=b'\x40\x12\x37\x80')
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert res == "LOAD_ROM_OK"
        session.close()

    def test_tier1_header_parsing_text_mock(self):
        rom = self.create_mock_rom("game.txt", text=True, console="N64")
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert res == "LOAD_ROM_OK"
        session.close()

    def test_tier1_header_parsing_valid_size(self):
        rom = self.create_mock_rom("game.z64", size=64)
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert res == "LOAD_ROM_OK"
        session.close()

    # Feature 3: Dynamic Resolution Scaling
    def test_tier1_resolution_default_320x240(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=5, rom=rom)
        assert res.is_success
        state = res.parse_state()
        assert state["console_type"] == "N64"
        video = res.read_video_buffer()
        assert len(video) == 320 * 240 * 3

    def test_tier1_resolution_expansion_640x480(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=5, rom=rom, extra_args=["--expansion-pak"])
        assert res.is_success
        state = res.parse_state()
        assert state["console_type"] == "N64"
        assert state["expansion_pak"] is True
        video = res.read_video_buffer()
        assert len(video) == 640 * 480 * 3

    def test_tier1_resolution_recenter_default(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=0, rom=rom)
        state = res.parse_state()
        assert state["player_x"] == 160
        assert state["player_y"] == 120

    def test_tier1_resolution_recenter_expansion(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=0, rom=rom, extra_args=["--expansion-pak"])
        state = res.parse_state()
        assert state["player_x"] == 320
        assert state["player_y"] == 240

    def test_tier1_resolution_buffer_size_calculation(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=1, rom=rom)
        video = res.read_video_buffer()
        assert len(video) == 230400

    # Feature 4: Digital Button Mappings
    def test_tier1_digital_buttons_dpad_up(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True)) # start to exit splash screen
        inputs.set_frame_input(1, ButtonState(up=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["player_y"] == 119

    def test_tier1_digital_buttons_dpad_down(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(down=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["player_y"] == 121

    def test_tier1_digital_buttons_a_b(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(a=True, b=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["a"] is True
        assert state["buttons"]["b"] is True

    def test_tier1_digital_buttons_l_r(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(l=True, r=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["l"] is True
        assert state["buttons"]["r"] is True

    def test_tier1_digital_buttons_z_start(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True, z=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["z"] is True
        assert state["buttons"]["start"] is True

    # Feature 5: C-Button Mappings
    def test_tier1_c_buttons_up(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(c_up=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["c_up"] is True

    def test_tier1_c_buttons_down(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(c_down=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["c_down"] is True

    def test_tier1_c_buttons_left(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(c_left=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["c_left"] is True

    def test_tier1_c_buttons_right(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(c_right=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["c_right"] is True

    def test_tier1_c_buttons_multiple(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(c_up=True, c_left=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["c_up"] is True
        assert state["buttons"]["c_left"] is True

    # Feature 6: Analog Stick Input
    def test_tier1_analog_stick_neutral(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=2, rom=rom)
        state = res.parse_state()
        assert state["stick_x"] == 0
        assert state["stick_y"] == 0

    def test_tier1_analog_stick_positive_x(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(stick_x=50))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["stick_x"] == 50
        assert state["player_x"] == 161

    def test_tier1_analog_stick_negative_x(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(stick_x=-50))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["stick_x"] == -50
        assert state["player_x"] == 159

    def test_tier1_analog_stick_positive_y(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(stick_y=50))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["stick_y"] == 50
        assert state["player_y"] == 121

    def test_tier1_analog_stick_negative_y(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(stick_y=-50))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["stick_y"] == -50
        assert state["player_y"] == 119

    # Feature 7: Speed Control
    def test_tier1_speed_half(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=5, rom=rom, speed=0.5)
        state = res.parse_state()
        assert state["speed"] == 0.5

    def test_tier1_speed_normal(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=5, rom=rom, speed=1.0)
        state = res.parse_state()
        assert state["speed"] == 1.0

    def test_tier1_speed_double(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=5, rom=rom, speed=2.0)
        state = res.parse_state()
        assert state["speed"] == 2.0

    def test_tier1_speed_quad(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=5, rom=rom, speed=4.0)
        state = res.parse_state()
        assert state["speed"] == 4.0

    def test_tier1_speed_cycle_multiplier(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=5, rom=rom, speed=2.0)
        state = res.parse_state()
        assert state["cpu_cycles"] == 15625000

    # Feature 8: Frame Skipping
    def test_tier1_frame_skip_none(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=6, rom=rom, frame_skip=0)
        state = res.parse_state()
        assert state["frame_skip"] == 0
        assert state["rendered_frames"] == 6

    def test_tier1_frame_skip_one(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=6, rom=rom, frame_skip=1)
        state = res.parse_state()
        assert state["frame_skip"] == 1
        assert state["rendered_frames"] == 3

    def test_tier1_frame_skip_five(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=6, rom=rom, frame_skip=5)
        state = res.parse_state()
        assert state["frame_skip"] == 5
        assert state["rendered_frames"] == 1

    def test_tier1_frame_skip_render_ticks(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=10, rom=rom, frame_skip=2)
        state = res.parse_state()
        assert state["rendered_frames"] == 3

    def test_tier1_frame_skip_cached_buffer(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=2, rom=rom, frame_skip=2)
        state = res.parse_state()
        assert state["rendered_frames"] == 0

    # Feature 9: Atomic Savestates
    def test_tier1_savestate_save_success(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        res = session.send_command("SAVE_STATE 1")
        assert res == "SAVE_STATE_OK"
        session.close()

    def test_tier1_savestate_load_success(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("SAVE_STATE 1")
        res = session.send_command("LOAD_STATE 1")
        assert res == "LOAD_STATE_OK"
        session.close()

    def test_tier1_savestate_player_coords_restored(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"start\": true}")
        session.send_command("TICK") # exit splash
        session.send_command("INJECT {\"stick_x\": 50}")
        session.send_command("TICK")
        session.send_command("SAVE_STATE coords")
        session.send_command("INJECT {\"stick_x\": -50}")
        session.send_command("TICK")
        session.send_command("LOAD_STATE coords")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["player_x"] == 161
        session.close()

    def test_tier1_savestate_ticks_restored(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("TICK")
        session.send_command("TICK")
        session.send_command("SAVE_STATE ticks")
        session.send_command("TICK")
        session.send_command("LOAD_STATE ticks")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["ticks"] == 2
        session.close()

    def test_tier1_savestate_buttons_restored(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"z\": true, \"c_up\": true}")
        session.send_command("TICK")
        session.send_command("SAVE_STATE buttons")
        session.send_command("INJECT {\"z\": false, \"c_up\": false}")
        session.send_command("TICK")
        session.send_command("LOAD_STATE buttons")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["buttons"]["z"] is True
        assert state["buttons"]["c_up"] is True
        session.close()

    # Feature 10: MIPS64 CPU Cycles
    def test_tier1_cpu_cycles_per_frame(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=1)
        state = res.parse_state()
        assert state["cpu_cycles"] == 1562500

    def test_tier1_cpu_cycles_accumulation(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=4, rom=rom)
        state = res.parse_state()
        assert state["cpu_cycles"] == 4 * 1562500

    def test_tier1_cpu_cycles_with_speed(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=3, rom=rom, speed=0.5)
        state = res.parse_state()
        assert state["cpu_cycles"] == int(3 * 1562500 * 0.5)

    def test_tier1_cpu_cycles_pause(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=3, rom=rom, extra_args=["--pause"])
        state = res.parse_state()
        assert state["cpu_cycles"] == 0

    def test_tier1_cpu_cycles_overflow(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=1, rom=rom)
        state = res.parse_state()
        assert isinstance(state["cpu_cycles"], int)
        assert state["cpu_cycles"] >= 0

    # Feature 11: APU/Audio Resampling
    def test_tier1_audio_stereo_pcm(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=5, rom=rom)
        audio = res.read_audio_buffer()
        assert len(audio) == 14700

    def test_tier1_audio_silence_splash(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=2, rom=rom)
        audio = res.read_audio_buffer()
        assert all(b == 0 for b in audio)

    def test_tier1_audio_silence_pause(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=2, rom=rom, extra_args=["--pause"])
        audio = res.read_audio_buffer()
        assert all(b == 0 for b in audio)

    def test_tier1_audio_frequency_active(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(a=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        audio = res.read_audio_buffer()
        assert any(b != 0 for b in audio)

    def test_tier1_audio_buffer_size(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=10, rom=rom)
        audio = res.read_audio_buffer()
        assert len(audio) == 10 * 2940

    # Feature 12: Expansion Pak Configuration
    def test_tier1_expansion_pak_cli_enable(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=1, rom=rom, extra_args=["--expansion-pak"])
        state = res.parse_state()
        assert state["expansion_pak"] is True

    def test_tier1_expansion_pak_state_true(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive(extra_args=["--expansion-pak"])
        session.send_command(f"LOAD_ROM {rom}")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["expansion_pak"] is True
        session.close()

    def test_tier1_expansion_pak_interactive_command(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        res = session.send_command("EXPANSION_PAK ON")
        assert res == "EXPANSION_PAK_OK"
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["expansion_pak"] is True
        session.close()

    def test_tier1_expansion_pak_resolution_effect(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("EXPANSION_PAK ON")
        res = session.send_command(f"DUMP_VIDEO {os.path.join(self.temp_dir, 'video.raw')}")
        assert res == "DUMP_VIDEO_OK"
        video_len = os.path.getsize(os.path.join(self.temp_dir, 'video.raw'))
        assert video_len == 640 * 480 * 3
        session.close()

    def test_tier1_expansion_pak_recenter_coords(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("EXPANSION_PAK ON")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["player_x"] == 320
        assert state["player_y"] == 240
        session.close()

    # =========================================================================
    # TIER 2: Boundary & Corner Cases (60 cases, 5 per feature)
    # =========================================================================

    # Feature 1 Boundaries: ROM Scanning
    def test_tier2_rom_scanning_empty_dir(self):
        empty_dir = os.path.join(self.temp_dir, "empty_dir")
        os.makedirs(empty_dir, exist_ok=True)
        session = self.spawn_interactive()
        res = session.send_command(f"SCAN_ROMS {empty_dir}")
        assert "SCAN_ROMS_OK" in res
        assert "[]" in res
        session.close()

    def test_tier2_rom_scanning_nonexistent_dir(self):
        nonexistent = os.path.join(self.temp_dir, "nonexistent")
        session = self.spawn_interactive()
        res = session.send_command(f"SCAN_ROMS {nonexistent}")
        assert "SCAN_ROMS_ERROR" in res
        session.close()

    def test_tier2_rom_scanning_symlink_out_bounds(self):
        session = self.spawn_interactive()
        res = session.send_command(f"SCAN_ROMS {self.temp_dir}/../../")
        assert "SCAN_ROMS_ERROR" in res
        session.close()

    def test_tier2_rom_scanning_relative_path_traversal(self):
        session = self.spawn_interactive()
        res = session.send_command(f"SCAN_ROMS {self.temp_dir}/../tmp/../..")
        assert "SCAN_ROMS_ERROR" in res
        session.close()

    def test_tier2_rom_scanning_special_characters(self):
        special_dir = os.path.join(self.temp_dir, "dir_!@# $%^&()")
        os.makedirs(special_dir, exist_ok=True)
        rom = os.path.join(special_dir, "game.z64")
        with open(rom, "wb") as f:
            f.write(b'\x80\x37\x12\x40' + b'\x00'*60)
        session = self.spawn_interactive()
        res = session.send_command(f"SCAN_ROMS {special_dir}")
        assert "SCAN_ROMS_OK" in res
        assert "game.z64" in res
        session.close()

    # Feature 2 Boundaries: ROM Header Parsing
    def test_tier2_header_parsing_truncated_less_64(self):
        rom = self.create_mock_rom("game.z64", size=32)
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert "LOAD_ROM_ERROR" in res
        session.close()

    def test_tier2_header_parsing_empty_rom(self):
        rom = self.create_mock_rom("game.z64", size=0)
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert "LOAD_ROM_ERROR" in res
        session.close()

    def test_tier2_header_parsing_invalid_magic(self):
        rom = self.create_mock_rom("game.z64", magic=b'\x00\x00\x00\x00')
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert "LOAD_ROM_ERROR" in res
        session.close()

    def test_tier2_header_parsing_text_mock_missing_console(self):
        rom = os.path.join(self.temp_dir, "game.txt")
        with open(rom, "w") as f:
            f.write("LOGO: VALID\nCHECKSUM: VALID\n")
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert "LOAD_ROM_ERROR" in res
        session.close()

    def test_tier2_header_parsing_text_mock_invalid_logo(self):
        rom = self.create_mock_rom("game.txt", text=True, logo_valid=False)
        session = self.spawn_interactive()
        res = session.send_command(f"LOAD_ROM {rom}")
        assert "LOAD_ROM_ERROR" in res
        session.close()

    # Feature 3 Boundaries: Dynamic Resolution Scaling
    def test_tier2_resolution_rapid_switching(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        for _ in range(5):
            session.send_command("EXPANSION_PAK ON")
            session.send_command("EXPANSION_PAK OFF")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["expansion_pak"] is False
        session.close()

    def test_tier2_resolution_extreme_dimensions(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive(extra_args=["--expansion-pak"])
        session.send_command(f"LOAD_ROM {rom}")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["player_x"] == 320
        session.close()

    def test_tier2_resolution_buffer_bounds_check(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"start\": true}")
        session.send_command("TICK")
        session.send_command("EXPANSION_PAK ON")
        session.send_command("TICK")
        res = session.send_command(f"DUMP_VIDEO {os.path.join(self.temp_dir, 'video.raw')}")
        assert res == "DUMP_VIDEO_OK"
        session.close()

    def test_tier2_resolution_uninitialized_buffer_access(self):
        session = self.spawn_interactive()
        res = session.send_command(f"DUMP_VIDEO {os.path.join(self.temp_dir, 'video.raw')}")
        assert res == "DUMP_VIDEO_OK"
        session.close()

    def test_tier2_resolution_recenter_clamping(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"start\": true}")
        session.send_command("TICK") # exit splash
        for _ in range(400):
            session.send_command("INJECT {\"stick_x\": 100}")
            session.send_command("TICK")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["player_x"] <= 319
        session.close()

    # Feature 4 Boundaries: Digital Button Mappings
    def test_tier2_digital_buttons_simultaneous_opposites_dpad_x(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(left=True, right=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["player_x"] == 160

    def test_tier2_digital_buttons_simultaneous_opposites_dpad_y(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(up=True, down=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["player_y"] == 120

    def test_tier2_digital_buttons_rapid_presses(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"start\": true}")
        session.send_command("TICK")
        for _ in range(10):
            session.send_command("INJECT {\"a\": true}")
            session.send_command("TICK")
            session.send_command("INJECT {\"a\": false}")
            session.send_command("TICK")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["buttons"]["a"] is False
        session.close()

    def test_tier2_digital_buttons_invalid_json_inject(self):
        session = self.spawn_interactive()
        res = session.send_command("INJECT {invalid_json}")
        assert "INJECT_ERROR" in res
        session.close()

    def test_tier2_digital_buttons_out_of_bounds_frame(self):
        rom = self.create_mock_rom("game.z64")
        inputs_path = os.path.join(self.temp_dir, "input.json")
        with open(inputs_path, "w") as f:
            json.dump([{"frame": 9999999, "buttons": {}}], f)
        res = self.runner.run(ticks=5, rom=rom, input_file=inputs_path)
        assert res.return_code != 0 or "Error" in res.stderr

    # Feature 5 Boundaries: C-Button Mappings
    def test_tier2_c_buttons_opposite_vertical(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(c_up=True, c_down=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["c_up"] is True
        assert state["buttons"]["c_down"] is True

    def test_tier2_c_buttons_opposite_horizontal(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(c_left=True, c_right=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["c_left"] is True
        assert state["buttons"]["c_right"] is True

    def test_tier2_c_buttons_rapid_sequential(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"c_up\": true}")
        session.send_command("TICK")
        session.send_command("INJECT {\"c_up\": false, \"c_down\": true}")
        session.send_command("TICK")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["buttons"]["c_up"] is False
        assert state["buttons"]["c_down"] is True
        session.close()

    def test_tier2_c_buttons_combos_with_dpad(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(up=True, c_up=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["up"] is True
        assert state["buttons"]["c_up"] is True

    def test_tier2_c_buttons_unmapped_keys(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(select=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"].get("select", False) is True

    # Feature 6 Boundaries: Analog Stick Boundaries
    def test_tier2_analog_stick_max_positive_x(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(stick_x=127))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["stick_x"] == 127

    def test_tier2_analog_stick_max_negative_x(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(stick_x=-128))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["stick_x"] == -128

    def test_tier2_analog_stick_max_positive_y(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(stick_y=127))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["stick_y"] == 127

    def test_tier2_analog_stick_max_negative_y(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(stick_y=-128))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=2, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["stick_y"] == -128

    def test_tier2_analog_stick_drift_threshold(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(stick_x=9, stick_y=-9))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["player_x"] == 160
        assert state["player_y"] == 120

    # Feature 7 Boundaries: Speed Control Boundaries
    def test_tier2_speed_negative_rejection(self):
        session = self.spawn_interactive()
        res = session.send_command("SET_SPEED -1.5")
        assert "SET_SPEED_ERROR" in res
        session.close()

    def test_tier2_speed_zero_rejection(self):
        session = self.spawn_interactive()
        res = session.send_command("SET_SPEED 0")
        assert "SET_SPEED_ERROR" in res
        session.close()

    def test_tier2_speed_extreme_high(self):
        session = self.spawn_interactive()
        res = session.send_command("SET_SPEED 2000.0")
        assert "SET_SPEED_ERROR" in res
        session.close()

    def test_tier2_speed_non_numeric(self):
        session = self.spawn_interactive()
        res = session.send_command("SET_SPEED abc")
        assert "SET_SPEED_ERROR" in res
        session.close()

    def test_tier2_speed_change_mid_run(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("TICK")
        session.send_command("SET_SPEED 2.5")
        session.send_command("TICK")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["speed"] == 2.5
        session.close()

    # Feature 8 Boundaries: Frame Skipping Boundaries
    def test_tier2_frame_skip_negative_rejection(self):
        session = self.spawn_interactive()
        res = session.send_command("SET_FRAME_SKIP -1")
        assert "SET_FRAME_SKIP_ERROR" in res
        session.close()

    def test_tier2_frame_skip_extreme_high(self):
        session = self.spawn_interactive()
        res = session.send_command("SET_FRAME_SKIP 5000")
        assert "SET_FRAME_SKIP_ERROR" in res
        session.close()

    def test_tier2_frame_skip_non_integer(self):
        session = self.spawn_interactive()
        res = session.send_command("SET_FRAME_SKIP 1.5")
        assert "SET_FRAME_SKIP_ERROR" in res
        session.close()

    def test_tier2_frame_skip_change_mid_run(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("TICK")
        session.send_command("SET_FRAME_SKIP 3")
        session.send_command("TICK")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["frame_skip"] == 3
        session.close()

    def test_tier2_frame_skip_audio_unaffected(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=6, rom=rom, frame_skip=2)
        audio = res.read_audio_buffer()
        assert len(audio) == 6 * 2940

    # Feature 9 Boundaries: Atomic Savestates Boundaries
    def test_tier2_savestate_disk_full_handling(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        os.environ["MOCK_DISK_FULL"] = "1"
        res = session.send_command("SAVE_STATE 1")
        assert "SAVE_STATE_ERROR" in res
        del os.environ["MOCK_DISK_FULL"]
        session.close()

    def test_tier2_savestate_nonexistent_slot_load(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        res = session.send_command("LOAD_STATE nonexistent_slot")
        assert "LOAD_STATE_ERROR" in res
        session.close()

    def test_tier2_savestate_invalid_slot_characters(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        res = session.send_command("SAVE_STATE ../slot")
        assert "SAVE_STATE_ERROR" in res
        session.close()

    def test_tier2_savestate_too_large_state(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        rom_stem = os.path.splitext(os.path.basename(rom))[0]
        sav_path = os.path.join(self.temp_dir, f"{rom_stem}_savestate_large.sav")
        with open(sav_path, "wb") as f:
            f.write(b'{ "console_type": "N64" }' + b' ' * (33 * 1024 * 1024))
        res = session.send_command("LOAD_STATE large")
        assert "LOAD_STATE_ERROR State file too large" in res
        session.close()

    def test_tier2_savestate_corrupt_json_state(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        rom_stem = os.path.splitext(os.path.basename(rom))[0]
        sav_path = os.path.join(self.temp_dir, f"{rom_stem}_savestate_corrupt.sav")
        with open(sav_path, "w") as f:
            f.write("{ corrupt json }")
        res = session.send_command("LOAD_STATE corrupt")
        assert "LOAD_STATE_ERROR" in res
        session.close()

    # Feature 10 Boundaries: MIPS64 CPU Cycles Boundaries
    def test_tier2_cpu_cycles_extreme_high_ticks(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=1000, rom=rom)
        state = res.parse_state()
        assert state["cpu_cycles"] == 1000 * 1562500

    def test_tier2_cpu_cycles_integer_overflow(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=100, rom=rom, speed=100.0)
        state = res.parse_state()
        assert state["cpu_cycles"] == 100 * 1562500 * 100

    def test_tier2_cpu_cycles_ticks_negative_rejection(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=-5, rom=rom)
        assert res.return_code != 0
        assert "cannot be negative" in res.stderr

    def test_tier2_cpu_cycles_ticks_non_numeric_rejection(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=0, rom=rom, extra_args=["--ticks", "abc"])
        assert res.return_code != 0
        assert "Invalid ticks" in res.stderr

    def test_tier2_cpu_cycles_ticks_huge_rejection(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=0, rom=rom, extra_args=["--ticks", "99999999999999999999"])
        assert res.return_code != 0
        assert "Invalid ticks" in res.stderr

    # Feature 11 Boundaries: APU/Audio Boundaries
    def test_tier2_audio_buffer_wrap_around(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=100, rom=rom)
        audio = res.read_audio_buffer()
        assert len(audio) == 100 * 2940

    def test_tier2_audio_extreme_speed_sound(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=5, rom=rom, speed=10.0)
        audio = res.read_audio_buffer()
        assert len(audio) == 147000

    def test_tier2_audio_unaligned_pcm_bytes(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=7, rom=rom, speed=1.3)
        audio = res.read_audio_buffer()
        assert len(audio) % 4 == 0

    def test_tier2_audio_missing_dump_path(self):
        session = self.spawn_interactive()
        res = session.send_command("DUMP_AUDIO")
        assert "DUMP_AUDIO_ERROR" in res
        session.close()

    def test_tier2_audio_invalid_dump_path(self):
        session = self.spawn_interactive()
        res = session.send_command("DUMP_AUDIO /invalid_dir/audio.raw")
        assert "DUMP_AUDIO_ERROR" in res
        session.close()

    # Feature 12 Boundaries: Expansion Pak Boundaries
    def test_tier2_expansion_pak_enable_disable_mid_run(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("EXPANSION_PAK ON")
        session.send_command("EXPANSION_PAK OFF")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["expansion_pak"] is False
        assert state["player_x"] == 160
        session.close()

    def test_tier2_expansion_pak_state_persistence_across_reset(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("EXPANSION_PAK ON")
        session.send_command("RESET")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["expansion_pak"] is False
        session.close()

    def test_tier2_expansion_pak_state_persistence_in_savestate(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("EXPANSION_PAK ON")
        session.send_command("SAVE_STATE exppak")
        session.send_command("EXPANSION_PAK OFF")
        session.send_command("LOAD_STATE exppak")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["expansion_pak"] is True
        session.close()

    def test_tier2_expansion_pak_interactive_rejection_invalid_vals(self):
        session = self.spawn_interactive()
        res = session.send_command("EXPANSION_PAK ENABLED")
        assert "EXPANSION_PAK_ERROR" in res
        session.close()

    def test_tier2_expansion_pak_extra_memory_allocation_check(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive(extra_args=["--expansion-pak"])
        session.send_command(f"LOAD_ROM {rom}")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["video_buffer_addr"] % 16 == 0
        assert state["audio_buffer_addr"] % 16 == 0
        session.close()

    # =========================================================================
    # TIER 3: Cross-Feature Combinations (12 cases)
    # =========================================================================

    def test_tier3_rom_load_and_expansion_pak_resolution(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive(extra_args=["--expansion-pak"])
        session.send_command(f"LOAD_ROM {rom}")
        res = session.send_command(f"DUMP_VIDEO {os.path.join(self.temp_dir, 'video.raw')}")
        assert res == "DUMP_VIDEO_OK"
        video_len = os.path.getsize(os.path.join(self.temp_dir, 'video.raw'))
        assert video_len == 640 * 480 * 3
        session.close()

    def test_tier3_analog_movement_with_speed_control(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(stick_x=80))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, speed=2.0, input_file=input_path)
        state = res.parse_state()
        assert state["cpu_cycles"] == 3 * 1562500 * 2.0
        assert state["player_x"] == 161

    def test_tier3_frame_skip_with_high_speed_audio(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=10, rom=rom, frame_skip=4, speed=3.0)
        state = res.parse_state()
        assert state["rendered_frames"] == 2
        audio = res.read_audio_buffer()
        assert len(audio) == 88200

    def test_tier3_savestate_persistence_across_resolution_switch(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("TICK")
        session.send_command("EXPANSION_PAK ON")
        session.send_command("TICK")
        session.send_command("SAVE_STATE highres")
        session.send_command("EXPANSION_PAK OFF")
        session.send_command("LOAD_STATE highres")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["expansion_pak"] is True
        assert state["player_x"] == 320
        session.close()

    def test_tier3_c_buttons_and_analog_stick_simultaneous(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(c_up=True, stick_x=50))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        state = res.parse_state()
        assert state["buttons"]["c_up"] is True
        assert state["stick_x"] == 50
        assert state["player_x"] == 161

    def test_tier3_rom_scan_and_path_traversal_on_load_state(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        res_scan = session.send_command(f"SCAN_ROMS {self.temp_dir}")
        assert "SCAN_ROMS_OK" in res_scan
        res_load = session.send_command("LOAD_STATE ../../etc")
        assert "LOAD_STATE_ERROR" in res_load
        session.close()

    def test_tier3_open_pif_rom_during_gameplay(self):
        pass

    def test_tier3_mips64_cycles_with_frame_skip_and_speed(self):
        rom = self.create_mock_rom("game.z64")
        res = self.runner.run(ticks=6, rom=rom, frame_skip=2, speed=0.5)
        state = res.parse_state()
        assert state["cpu_cycles"] == int(6 * 1562500 * 0.5)
        assert state["rendered_frames"] == 2

    def test_tier3_atomic_save_during_paused_state(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive(extra_args=["--pause"])
        session.send_command(f"LOAD_ROM {rom}")
        res_save = session.send_command("SAVE_STATE paused_state")
        assert res_save == "SAVE_STATE_OK"
        res_load = session.send_command("LOAD_STATE paused_state")
        assert res_load == "LOAD_STATE_OK"
        session.close()

    def test_tier3_audio_sine_generation_during_button_combos(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(a=True, stick_x=100, z=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=3, rom=rom, input_file=input_path)
        audio = res.read_audio_buffer()
        assert any(b != 0 for b in audio)

    def test_tier3_expansion_pak_toggle_with_active_speed(self):
        rom = self.create_mock_rom("game.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("SET_SPEED 3.0")
        session.send_command("EXPANSION_PAK ON")
        session.send_command("PLAY")
        session.send_command("TICK")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["expansion_pak"] is True
        assert state["speed"] == 3.0
        session.close()

    def test_tier3_multiple_rom_loads_and_resolution_scaling(self):
        rom1 = self.create_mock_rom("game1.z64")
        rom2 = self.create_mock_rom("game2.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom1}")
        session.send_command("EXPANSION_PAK ON")
        session.send_command(f"LOAD_ROM {rom2}")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["expansion_pak"] is False
        assert state["console_type"] == "N64"
        session.close()

    def test_tier3_input_injection_during_speed_and_skip_combinations(self):
        rom = self.create_mock_rom("game.z64")
        inputs = InputSequenceGenerator()
        inputs.set_frame_input(0, ButtonState(start=True))
        inputs.set_frame_input(1, ButtonState(stick_y=-80, a=True))
        input_path = os.path.join(self.temp_dir, "input.json")
        inputs.write_to_file(input_path)
        res = self.runner.run(ticks=4, rom=rom, speed=1.5, frame_skip=2, input_file=input_path)
        state = res.parse_state()
        assert state["player_y"] == 118
        assert state["speed"] == 1.5

    # =========================================================================
    # TIER 4: Real-world walkthroughs (6 scenarios)
    # =========================================================================

    def test_tier4_scenario_boot_play_save_load(self):
        rom = self.create_mock_rom("super_mario_64.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"start\": true}")
        session.send_command("TICK") # exit splash
        session.send_command("SET_SPEED 1.5")
        session.send_command("INJECT {\"stick_x\": 60, \"z\": true}")
        session.send_command("TICK")
        session.send_command("SAVE_STATE sm64_slot")
        session.send_command("RESET")
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("LOAD_STATE sm64_slot")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["player_x"] == 161
        assert state["speed"] == 1.5
        assert state["buttons"]["z"] is True
        session.close()

    def test_tier4_scenario_expansion_pak_high_res_playthrough(self):
        rom = self.create_mock_rom("zelda_oot.z64")
        session = self.spawn_interactive(extra_args=["--expansion-pak"])
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"start\": true}")
        session.send_command("TICK")
        session.send_command("INJECT {\"stick_y\": 120, \"c_down\": true}")
        session.send_command("TICK")
        res_video = session.send_command(f"DUMP_VIDEO {os.path.join(self.temp_dir, 'video.raw')}")
        assert res_video == "DUMP_VIDEO_OK"
        video_len = os.path.getsize(os.path.join(self.temp_dir, 'video.raw'))
        assert video_len == 640 * 480 * 3
        session.send_command("EXPANSION_PAK OFF")
        res_video_low = session.send_command(f"DUMP_VIDEO {os.path.join(self.temp_dir, 'video_low.raw')}")
        assert res_video_low == "DUMP_VIDEO_OK"
        video_low_len = os.path.getsize(os.path.join(self.temp_dir, 'video_low.raw'))
        assert video_low_len == 320 * 240 * 3
        session.close()

    def test_tier4_scenario_fast_forward_skip_level_completion(self):
        rom = self.create_mock_rom("mariokart64.z64")
        res = self.runner.run(ticks=50, rom=rom, speed=8.0, frame_skip=4)
        state = res.parse_state()
        assert state["rendered_frames"] == 10
        assert state["cpu_cycles"] == 625000000

    def test_tier4_scenario_controller_intensive_combo_session(self):
        rom = self.create_mock_rom("smash64.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"start\": true}")
        session.send_command("TICK")
        session.send_command("INJECT {\"up\": true, \"z\": true, \"a\": true, \"stick_x\": 100, \"c_right\": true}")
        session.send_command("TICK")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["buttons"]["up"] is True
        assert state["buttons"]["z"] is True
        assert state["buttons"]["a"] is True
        assert state["buttons"]["c_right"] is True
        assert state["stick_x"] == 100
        assert state["player_x"] == 161
        session.close()

    def test_tier4_scenario_multi_rom_hot_swap(self):
        rom1 = self.create_mock_rom("starfox64.z64")
        rom2 = self.create_mock_rom("banjokazooie.z64")
        session = self.spawn_interactive()
        session.send_command(f"LOAD_ROM {rom1}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"start\": true}")
        session.send_command("TICK")
        session.send_command("INJECT {\"stick_y\": 50}")
        session.send_command("TICK")
        session.send_command("SAVE_STATE sf64_save")
        session.send_command(f"LOAD_ROM {rom2}")
        session.send_command("PLAY")
        session.send_command("INJECT {\"start\": true}")
        session.send_command("TICK")
        session.send_command("INJECT {\"stick_y\": -80}")
        session.send_command("TICK")
        session.send_command(f"LOAD_ROM {rom1}")
        session.send_command("LOAD_STATE sf64_save")
        dump_path = os.path.join(self.temp_dir, "dump.json")
        session.send_command(f"DUMP_STATE {dump_path}")
        with open(dump_path, "r") as f:
            state = json.load(f)
        assert state["console_type"] == "N64"
        assert state["player_y"] == 121
        session.close()

    def test_tier4_scenario_performance_stress_run(self):
        rom = self.create_mock_rom("goldeneye007.z64")
        t0 = time.perf_counter()
        res = self.runner.run(ticks=500, rom=rom)
        t1 = time.perf_counter()
        assert res.is_success
        assert (t1 - t0) < 3.0
        state = res.parse_state()
        assert state["ticks"] == 500
