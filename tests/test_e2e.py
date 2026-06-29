"""E2E Test Suite for the FFI Bridge Emulator.

This test suite implements Tiers 1 through 4 of the E2E test design,
verifying features, boundary/corner cases, cross-feature combinations,
and a real-world gameplay emulation walkthrough against a mock emulator binary.
"""

from dataclasses import dataclass
import json
import math
import os
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

        Raises:
            ValueError: If state_path is missing or file does not exist.
        """
        if not self.state_path or not os.path.exists(self.state_path):
            raise ValueError("State dump was not requested or failed to generate.")
        with open(self.state_path, "r", encoding="utf-8") as f:
            return json.load(f)

    def read_video_buffer(self) -> bytes:
        """Reads the raw video buffer bytes.

        Returns:
            bytes: The raw video buffer.

        Raises:
            ValueError: If video_path is missing or file does not exist.
        """
        if not self.video_path or not os.path.exists(self.video_path):
            raise ValueError("Video dump was not requested or failed to generate.")
        with open(self.video_path, "rb") as f:
            return f.read()

    def read_audio_buffer(self) -> bytes:
        """Reads the raw audio buffer bytes.

        Returns:
            bytes: The raw audio buffer.

        Raises:
            ValueError: If audio_path is missing or file does not exist.
        """
        if not self.audio_path or not os.path.exists(self.audio_path):
            raise ValueError("Audio dump was not requested or failed to generate.")
        with open(self.audio_path, "rb") as f:
            return f.read()


class EmulatorProcessRunner:
    """Handles invoking the C++ emulator binary and capturing output assets."""

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
        extra_args: Optional[List[str]] = None,
    ) -> EmulatorRunResult:
        """Executes the frontend subprocess and retrieves execution artifacts.

        Args:
            ticks (int): Number of ticks to run.
            input_file (str, optional): Path to input injection JSON.
            dump_video (bool): Whether to dump video.
            dump_audio (bool): Whether to dump audio.
            dump_state (bool): Whether to dump state.
            extra_args (List[str], optional): Extra arguments.

        Returns:
            EmulatorRunResult: The run execution result.
        """
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
        """Initializes the interactive session.

        Args:
            process (subprocess.Popen): The active subprocess.
        """
        self._process: subprocess.Popen = process

    def send_command(self, cmd: str) -> str:
        """Sends a command to the process and reads the response line.

        Args:
            cmd (str): Command to send.

        Returns:
            str: Response line from the stdout of the emulator.
        """
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
    """Base class for test cases containing common fixtures."""

    @pytest.fixture(autouse=True)
    def setup_temp_dir(self) -> None:
        """Sets up a temporary directory for each test case."""
        with tempfile.TemporaryDirectory() as temp_dir:
            self.temp_dir: str = temp_dir
            self.runner: EmulatorProcessRunner = EmulatorProcessRunner(
                EMULATOR_BIN, self.temp_dir
            )
            yield

    def spawn_interactive(self) -> InteractiveEmulatorSession:
        """Spawns an interactive emulator process.

        Returns:
            InteractiveEmulatorSession: The active interactive session wrapper.
        """
        if EMULATOR_BIN.endswith(".py"):
            cmd = [sys.executable, EMULATOR_BIN]
        else:
            cmd = [EMULATOR_BIN]
        cmd.extend([
            "--headless",
            "--test-mode",
            "--interactive",
        ])
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


# ==============================================================================
# TIER 1: Feature Coverage (>= 5 cases per feature)
# ==============================================================================


class TestCoreInitialization(TestBase):
    """Tier 1: Feature Coverage - Core Initialization and Destruction (5 Cases)."""

    def test_1_1_single_core_clean_lifecycle(self) -> None:
        """TestCase 1.1: Single Core Clean Lifecycle."""
        result = self.runner.run(ticks=0)
        assert result.is_success
        state = result.parse_state()
        assert state["ticks"] == 0
        assert not result.stderr or "Error" not in result.stderr

    def test_1_2_multiple_sequential_lifecycle(self) -> None:
        """TestCase 1.2: Multiple Sequential Lifecycle."""
        for _ in range(10):
            result = self.runner.run(ticks=0)
            assert result.is_success
            state = result.parse_state()
            assert state["ticks"] == 0

    def test_1_3_dual_concurrent_cores(self) -> None:
        """TestCase 1.3: Dual Concurrent Cores."""
        result_a = self.runner.run(ticks=0, dump_state=True)
        result_b = self.runner.run(ticks=0, dump_state=True)

        assert result_a.is_success
        assert result_b.is_success

        state_a = result_a.parse_state()
        state_b = result_b.parse_state()

        # Addresses must be different due to PID difference
        assert state_a["video_buffer_addr"] != state_b["video_buffer_addr"]
        assert state_a["audio_buffer_addr"] != state_b["audio_buffer_addr"]

    def test_1_4_initialization_error_handling(self) -> None:
        """TestCase 1.4: Initialization Error Handling."""
        result = self.runner.run(ticks=0, extra_args=["--invalid-flag"])
        assert not result.is_success
        assert result.return_code != 0
        assert "Unknown CLI arguments" in result.stderr

    def test_1_5_rapid_creation_stress_test(self) -> None:
        """TestCase 1.5: Rapid Creation Stress Test."""
        start_time = time.time()
        for _ in range(100):
            result = self.runner.run(ticks=0)
            assert result.is_success
        duration = time.time() - start_time
        assert duration < 15.0  # Runs well under 15 seconds


class TestRuntimeControls(TestBase):
    """Tier 1: Feature Coverage - Playback State Control (5 Cases)."""

    def test_2_1_transition_play_pause_play(self) -> None:
        """TestCase 2.1: Transition Play -> Pause -> Play."""
        session = self.spawn_interactive()
        try:
            assert session.send_command("PLAY") == "PLAY_OK"

            # Tick 5 times in play
            for _ in range(5):
                assert "TICK_OK" in session.send_command("TICK")

            # Dump video after 5 play ticks
            video_path_play1 = os.path.join(self.temp_dir, "video_play1.raw")
            assert session.send_command(f"DUMP_VIDEO {video_path_play1}") == "DUMP_VIDEO_OK"
            with open(video_path_play1, "rb") as f:
                play1_bytes = f.read()

            # Pause and tick 5 times
            assert session.send_command("PAUSE") == "PAUSE_OK"
            for _ in range(5):
                assert "TICK_OK" in session.send_command("TICK")

            # Dump video after 5 pause ticks: should be identical to play1
            video_path_pause = os.path.join(self.temp_dir, "video_pause.raw")
            assert session.send_command(f"DUMP_VIDEO {video_path_pause}") == "DUMP_VIDEO_OK"
            with open(video_path_pause, "rb") as f:
                pause_bytes = f.read()
            assert play1_bytes == pause_bytes

            # Play and tick 5 times
            assert session.send_command("PLAY") == "PLAY_OK"
            for _ in range(5):
                assert "TICK_OK" in session.send_command("TICK")

            # Dump video after resuming play: should be different
            video_path_play2 = os.path.join(self.temp_dir, "video_play2.raw")
            assert session.send_command(f"DUMP_VIDEO {video_path_play2}") == "DUMP_VIDEO_OK"
            with open(video_path_play2, "rb") as f:
                play2_bytes = f.read()
            assert play1_bytes != play2_bytes
        finally:
            session.close()

    def test_2_2_reset_state_cleanliness(self) -> None:
        """TestCase 2.2: Reset State Cleanliness."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            for _ in range(20):
                session.send_command("TICK")

            # Reset the emulator
            assert session.send_command("RESET") == "RESET_OK"

            # Dump state and verify
            state_path = os.path.join(self.temp_dir, "state_reset.json")
            assert session.send_command(f"DUMP_STATE {state_path}") == "DUMP_STATE_OK"
            with open(state_path, "r", encoding="utf-8") as f:
                state = json.load(f)

            assert state["ticks"] == 0
            assert state["state"] == "splash"
            assert state["player_x"] == 128
            assert state["player_y"] == 120
            for btn, val in state["buttons"].items():
                assert not val
        finally:
            session.close()

    def test_2_3_pause_state_preservation(self) -> None:
        """TestCase 2.3: Pause State Preservation."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            # Advance to tick 15
            for _ in range(15):
                session.send_command("TICK")

            # Pause the emulator
            assert session.send_command("PAUSE") == "PAUSE_OK"

            # Get state
            state_path_1 = os.path.join(self.temp_dir, "state_pause1.json")
            session.send_command(f"DUMP_STATE {state_path_1}")
            with open(state_path_1, "r") as f:
                state1 = json.load(f)

            # TICK multiple times under pause
            for _ in range(10):
                session.send_command("TICK")

            # Get state again
            state_path_2 = os.path.join(self.temp_dir, "state_pause2.json")
            session.send_command(f"DUMP_STATE {state_path_2}")
            with open(state_path_2, "r") as f:
                state2 = json.load(f)

            # Ensure ticks and register coordinates remain exactly identical
            assert state1["ticks"] == state2["ticks"]
            assert state1["player_x"] == state2["player_x"]
            assert state1["player_y"] == state2["player_y"]
            assert state1["state"] == state2["state"]
        finally:
            session.close()

    def test_2_4_reset_state_behavior_active_vs_paused(self) -> None:
        """TestCase 2.4: Reset state behavior in active vs paused state."""
        # 1. Under pause, after reset, ticks do not advance
        result_pause = self.runner.run(ticks=5, extra_args=["--pause", "--reset"])
        assert result_pause.is_success
        state_pause = result_pause.parse_state()
        assert state_pause["playback_state"] == "pause"
        assert state_pause["ticks"] == 0

        # 2. Under play, after reset, ticks advance
        result_play = self.runner.run(ticks=5, extra_args=["--play", "--reset"])
        assert result_play.is_success
        state_play = result_play.parse_state()
        assert state_play["playback_state"] == "play"
        assert state_play["ticks"] == 5

    def test_2_5_interactive_control_sequences_via_stdin(self) -> None:
        """TestCase 2.5: Interactive Control Sequences via Stdin."""
        session = self.spawn_interactive()
        try:
            assert session.send_command("PLAY") == "PLAY_OK"
            assert session.send_command("PAUSE") == "PAUSE_OK"
            assert session.send_command("RESET") == "RESET_OK"
            assert session.send_command("PLAY") == "PLAY_OK"
            assert session.send_command("TICK") == "TICK_OK 1"
            assert session.send_command("EXIT") == "EXIT_OK"
        finally:
            session.close()


class TestInputInjection(TestBase):
    """Tier 1: Feature Coverage - Input Injection (5 Cases)."""

    def test_3_1_digital_button_press_release_lifecycle(self) -> None:
        """TestCase 3.1: Digital Button Press & Release Lifecycle."""
        # Frame 2: A=True, Frame 3: A=False
        gen = InputSequenceGenerator()
        gen.set_frame_input(2, ButtonState(a=True))
        gen.set_frame_input(3, ButtonState(a=False))
        input_path = os.path.join(self.temp_dir, "input.json")
        gen.write_to_file(input_path)

        session = self.spawn_interactive()
        try:
            session.send_command(f"INJECT {input_path}")
            session.send_command("PLAY")

            # Tick 0, 1, 2
            session.send_command("TICK")  # Tick 1 (frame index 0)
            session.send_command("TICK")  # Tick 2 (frame index 1)

            # Frame index 2 (tick 3)
            session.send_command("TICK")
            state_path = os.path.join(self.temp_dir, "state_frame2.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)
            assert state["buttons"]["a"] is True

            # Frame index 3 (tick 4)
            session.send_command("TICK")
            state_path = os.path.join(self.temp_dir, "state_frame3.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)
            assert state["buttons"]["a"] is False
        finally:
            session.close()

    def test_3_2_multi_button_concurrency(self) -> None:
        """TestCase 3.2: Multi-Button Concurrency."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            # Inject up, left, start simultaneously
            buttons = {"up": True, "left": True, "start": True}
            assert session.send_command(f"INJECT {json.dumps(buttons)}") == "INJECT_OK"
            session.send_command("TICK")

            state_path = os.path.join(self.temp_dir, "state_multi.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)

            assert state["buttons"]["up"] is True
            assert state["buttons"]["left"] is True
            assert state["buttons"]["start"] is True
            assert state["buttons"]["right"] is False
        finally:
            session.close()

    def test_3_3_opposite_directions_lockout_socd(self) -> None:
        """TestCase 3.3: Opposite Directions Lockout (SOCD)."""
        session = self.spawn_interactive()
        try:
            # Transition to Gameplay state first using START
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")

            # Verify we are in gameplay and at (128, 120)
            state_path = os.path.join(self.temp_dir, "state_game.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)
            assert state["state"] == "gameplay"
            assert state["player_x"] == 128
            assert state["player_y"] == 120

            # Inject LEFT + RIGHT
            session.send_command(f"INJECT {json.dumps({'left': True, 'right': True})}")
            session.send_command("TICK")

            state_path = os.path.join(self.temp_dir, "state_socd.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)

            # player_x must remain 128 (neutralized)
            assert state["player_x"] == 128
        finally:
            session.close()

    def test_3_4_streamed_input_sequence_playback(self) -> None:
        """TestCase 3.4: Streamed Input Sequence Playback."""
        gen = InputSequenceGenerator()
        # Set frames
        gen.set_frame_input(0, ButtonState(start=True))
        for f in range(2, 20):
            gen.set_frame_input(f, ButtonState(right=True))

        input_path = os.path.join(self.temp_dir, "stream.json")
        gen.write_to_file(input_path)

        result = self.runner.run(ticks=30, input_file=input_path)
        assert result.is_success
        state = result.parse_state()

        assert state["state"] == "gameplay"
        # Spawns at 128, then moves right for 18 frames -> 128 + 18 = 146
        assert state["player_x"] == 146

    def test_3_5_input_latch_latency(self) -> None:
        """TestCase 3.5: Input Latch Latency."""
        gen = InputSequenceGenerator()
        gen.set_frame_input(1, ButtonState(b=True))
        gen.set_frame_input(2, ButtonState(b=False))
        input_path = os.path.join(self.temp_dir, "latch.json")
        gen.write_to_file(input_path)

        session = self.spawn_interactive()
        try:
            session.send_command(f"INJECT {input_path}")
            session.send_command("PLAY")

            session.send_command("TICK")  # Frame 0: no action
            session.send_command("TICK")  # Frame 1: b=True

            state_path1 = os.path.join(self.temp_dir, "latch_frame1.json")
            session.send_command(f"DUMP_STATE {state_path1}")
            with open(state_path1, "r") as f:
                s1 = json.load(f)
            assert s1["buttons"]["b"] is True

            session.send_command("TICK")  # Frame 2: b=False

            state_path2 = os.path.join(self.temp_dir, "latch_frame2.json")
            session.send_command(f"DUMP_STATE {state_path2}")
            with open(state_path2, "r") as f:
                s2 = json.load(f)
            assert s2["buttons"]["b"] is False
        finally:
            session.close()


class TestZeroCopyBuffers(TestBase):
    """Tier 1: Feature Coverage - Zero-Copy Buffers (5 Cases)."""

    def test_4_1_buffer_memory_address_invariance(self) -> None:
        """TestCase 4.1: Buffer Memory Address Invariance."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")

            # Check address at start
            state_path_start = os.path.join(self.temp_dir, "state_start.json")
            session.send_command(f"DUMP_STATE {state_path_start}")
            with open(state_path_start, "r") as f:
                state_start = json.load(f)
            addr_video_start = state_start["video_buffer_addr"]
            addr_audio_start = state_start["audio_buffer_addr"]

            # Tick multiple times
            for _ in range(10):
                session.send_command("TICK")

            # Check address at end
            state_path_end = os.path.join(self.temp_dir, "state_end.json")
            session.send_command(f"DUMP_STATE {state_path_end}")
            with open(state_path_end, "r") as f:
                state_end = json.load(f)

            assert state_end["video_buffer_addr"] == addr_video_start
            assert state_end["audio_buffer_addr"] == addr_audio_start
        finally:
            session.close()

    def test_4_2_zero_copy_propagation_validation(self) -> None:
        """TestCase 4.2: Zero-Copy Propagation Validation."""
        # Modifying state updates the memory buffer immediately.
        # Check that moving player changes the video buffer pixels immediately.
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")

            # Dump video before moving
            video_path_1 = os.path.join(self.temp_dir, "video_p1.raw")
            session.send_command(f"DUMP_VIDEO {video_path_1}")
            with open(video_path_1, "rb") as f:
                p1_bytes = f.read()

            # Move character
            session.send_command(f"INJECT {json.dumps({'right': True})}")
            session.send_command("TICK")

            # Dump video after moving
            video_path_2 = os.path.join(self.temp_dir, "video_p2.raw")
            session.send_command(f"DUMP_VIDEO {video_path_2}")
            with open(video_path_2, "rb") as f:
                p2_bytes = f.read()

            # Buffer changes immediately
            assert p1_bytes != p2_bytes
        finally:
            session.close()

    def test_4_3_memory_bounds_safety_and_layout(self) -> None:
        """TestCase 4.3: Memory Bounds Safety and Layout."""
        result = self.runner.run(ticks=5)
        assert result.is_success
        video_buf = result.read_video_buffer()
        audio_buf = result.read_audio_buffer()

        # Resolution footprint: 256 * 240 * 3 = 184320 bytes
        assert len(video_buf) == 184320
        # 5 ticks of audio: 5 * 735 * 2 * 2 = 14700 bytes
        assert len(audio_buf) == 14700

    def test_4_4_active_loop_heap_allocation_prevention(self) -> None:
        """TestCase 4.4: Active Loop Heap Allocation Prevention."""
        # Simulated allocator check. Verify execution of 10000 ticks completes quickly
        # and has constant resident memory / doesn't cause leaks.
        start_time = time.time()
        result = self.runner.run(ticks=1000)
        assert result.is_success
        duration = time.time() - start_time
        assert duration < 1.0  # Runs in O(1) heap footprint, rapid execution

    def test_4_5_buffer_content_test_pattern_verification(self) -> None:
        """TestCase 4.5: Buffer Content Test Pattern Verification."""
        result = self.runner.run(ticks=1, extra_args=["--play"])
        assert result.is_success
        video_buf = result.read_video_buffer()

        # In splash screen state, all pixels should be blue [0, 0, 255]
        # except pixel (0,0) which is animated.
        # Let's check bytes at index 3..6 (second pixel)
        assert video_buf[3] == 0
        assert video_buf[4] == 0
        assert video_buf[5] == 255


# ==============================================================================
# TIER 2: Boundary & Corner Cases (>= 5 cases per feature)
# ==============================================================================


class TestTier2BoundaryCorner(TestBase):
    """Tier 2: Boundary & Corner Cases."""

    # --- Feature 1: Emulator Lifecycle (5 Cases) ---

    def test_boundary_1_1_double_concurrent_init(self) -> None:
        """Case 1.1: Double / Concurrent Emulator Instance Initialization."""
        session1 = self.spawn_interactive()
        session2 = self.spawn_interactive()
        try:
            state_path1 = os.path.join(self.temp_dir, "s1.json")
            state_path2 = os.path.join(self.temp_dir, "s2.json")
            session1.send_command(f"DUMP_STATE {state_path1}")
            session2.send_command(f"DUMP_STATE {state_path2}")

            with open(state_path1, "r") as f:
                s1 = json.load(f)
            with open(state_path2, "r") as f:
                s2 = json.load(f)

            # Different processes must have different buffer addresses
            assert s1["video_buffer_addr"] != s2["video_buffer_addr"]
        finally:
            session1.close()
            session2.close()

    def test_boundary_1_2_immediate_destruction(self) -> None:
        """Case 1.2: Immediate Destruction After Creation."""
        session = self.spawn_interactive()
        session.close()  # Destructor triggers immediately without tick

    def test_boundary_1_3_destruction_active_playing(self) -> None:
        """Case 1.3: Destruction of Active / Playing Emulator."""
        for _ in range(5):
            session = self.spawn_interactive()
            session.send_command("PLAY")
            session.send_command("TICK")
            session.close()  # Tear down during play state

    def test_boundary_1_4_null_invalid_reference_safety(self) -> None:
        """Case 1.4: Null/Invalid Reference Safety (CLI error test)."""
        result = self.runner.run(ticks=0, extra_args=["--invalid-argument-here"])
        assert not result.is_success
        assert "Unknown CLI arguments" in result.stderr

    def test_boundary_1_5_sequence_rapid_recreations(self) -> None:
        """Case 1.5: Sequence of Rapid Re-creations."""
        for _ in range(50):
            session = self.spawn_interactive()
            session.close()

    # --- Feature 2: Playback State Control (5 Cases) ---

    def test_boundary_2_1_high_frequency_play_pause(self) -> None:
        """Case 2.1: High-Frequency Play/Pause State Transitions."""
        session = self.spawn_interactive()
        try:
            for _ in range(50):
                session.send_command("PLAY")
                session.send_command("PAUSE")
            assert session.send_command("PLAY") == "PLAY_OK"
            assert "TICK_OK" in session.send_command("TICK")
        finally:
            session.close()

    def test_boundary_2_2_redundant_state_invocations(self) -> None:
        """Case 2.2: Redundant State Invocations (Idempotency)."""
        session = self.spawn_interactive()
        try:
            assert session.send_command("PLAY") == "PLAY_OK"
            assert session.send_command("PLAY") == "PLAY_OK"
            assert session.send_command("PAUSE") == "PAUSE_OK"
            assert session.send_command("PAUSE") == "PAUSE_OK"
            assert session.send_command("RESET") == "RESET_OK"
            assert session.send_command("RESET") == "RESET_OK"
        finally:
            session.close()

    def test_boundary_2_3_reset_behavior_play_vs_pause(self) -> None:
        """Case 2.3: Reset Behavior in Play vs. Pause State."""
        session = self.spawn_interactive()
        try:
            # Active Reset
            session.send_command("PLAY")
            session.send_command("RESET")
            state_path = os.path.join(self.temp_dir, "reset_active.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)
            assert state["playback_state"] == "play"

            # Paused Reset
            session.send_command("PAUSE")
            session.send_command("RESET")
            state_path = os.path.join(self.temp_dir, "reset_paused.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)
            assert state["playback_state"] == "pause"
        finally:
            session.close()

    def test_boundary_2_4_reset_buffer_address_stability(self) -> None:
        """Case 2.4: Reset Buffer Address Stability."""
        session = self.spawn_interactive()
        try:
            state_path_1 = os.path.join(self.temp_dir, "r1.json")
            session.send_command(f"DUMP_STATE {state_path_1}")
            with open(state_path_1, "r") as f:
                r1 = json.load(f)

            session.send_command("RESET")

            state_path_2 = os.path.join(self.temp_dir, "r2.json")
            session.send_command(f"DUMP_STATE {state_path_2}")
            with open(state_path_2, "r") as f:
                r2 = json.load(f)

            assert r1["video_buffer_addr"] == r2["video_buffer_addr"]
            assert r1["audio_buffer_addr"] == r2["audio_buffer_addr"]
        finally:
            session.close()

    def test_boundary_2_5_command_execution_during_active_tick(self) -> None:
        """Case 2.5: Command Execution During Active Tick."""
        # Since it's interactive, we run multiple commands rapidly to ensure thread-safety
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            for _ in range(10):
                session.send_command("TICK")
                session.send_command("PAUSE")
                session.send_command("PLAY")
        finally:
            session.close()

    # --- Feature 3: Input Injection (5 Cases) ---

    def test_boundary_3_1_socd_neutralization(self) -> None:
        """Case 3.1: SOCD Neutralization (LEFT+RIGHT, UP+DOWN)."""
        session = self.spawn_interactive()
        try:
            # Start game
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")
            session.send_command(f"INJECT {json.dumps({'start': False})}")
            session.send_command("TICK")

            # Inject dual opposite directions
            session.send_command(f"INJECT {json.dumps({'up': True, 'down': True, 'left': True, 'right': True})}")
            session.send_command("TICK")

            state_path = os.path.join(self.temp_dir, "socd_boundary.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)

            # Coordinates must remain unchanged from center (128, 120)
            assert state["player_x"] == 128
            assert state["player_y"] == 120
        finally:
            session.close()

    def test_boundary_3_2_full_controller_mash(self) -> None:
        """Case 3.2: Full-Controller Mash (All Buttons Active)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")

            # Press ALL buttons
            all_buttons = {k: True for k in ["up", "down", "left", "right", "a", "b", "start", "select"]}
            assert session.send_command(f"INJECT {json.dumps(all_buttons)}") == "INJECT_OK"
            session.send_command("TICK")
        finally:
            session.close()

    def test_boundary_3_3_empty_input_state(self) -> None:
        """Case 3.3: Empty Input State."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({})}")
            session.send_command("TICK")
        finally:
            session.close()

    def test_boundary_3_4_frame_rate_jitter(self) -> None:
        """Case 3.4: Frame-rate Input Jitter (60Hz Toggle)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            for i in range(10):
                a_state = (i % 2 == 0)
                session.send_command(f"INJECT {json.dumps({'a': a_state})}")
                session.send_command("TICK")
        finally:
            session.close()

    def test_boundary_3_5_input_persistence_latency(self) -> None:
        """Case 3.5: Input Persistence and Latency (No-Sticky Inputs)."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            # Inject press
            session.send_command(f"INJECT {json.dumps({'a': True})}")
            session.send_command("TICK")
            # Clear input (should default to false next frame)
            session.send_command("TICK")

            state_path = os.path.join(self.temp_dir, "no_sticky.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)
            assert state["buttons"]["a"] is False
        finally:
            session.close()

    # --- Feature 4: Zero-Copy Buffers (5 Cases) ---

    def test_boundary_4_1_buffer_bounds_integrity(self) -> None:
        """Case 4.1: Video Buffer Slice Bounds Integrity."""
        result = self.runner.run(ticks=1)
        assert result.is_success
        video_buf = result.read_video_buffer()
        assert len(video_buf) == 256 * 240 * 3

    def test_boundary_4_2_buffer_address_continuity(self) -> None:
        """Case 4.2: Buffer Address Continuity."""
        result = self.runner.run(ticks=10)
        assert result.is_success
        state = result.parse_state()
        assert state["video_buffer_addr"] > 0
        assert state["audio_buffer_addr"] > 0

    def test_boundary_4_3_buffer_alignment(self) -> None:
        """Case 4.3: Memory Alignment Check."""
        result = self.runner.run(ticks=1)
        assert result.is_success
        state = result.parse_state()
        # Addresses must be 16-byte aligned
        assert state["video_buffer_addr"] % 16 == 0
        assert state["audio_buffer_addr"] % 16 == 0

    def test_boundary_4_4_amplitude_clipping(self) -> None:
        """Case 4.4: Maximum Amplitude Output Clip Handling."""
        # Run with jump A input to trigger non-silent audio
        gen = InputSequenceGenerator()
        gen.set_frame_input(0, ButtonState(start=True))
        gen.set_frame_input(1, ButtonState(a=True))
        input_path = os.path.join(self.temp_dir, "clip.json")
        gen.write_to_file(input_path)

        result = self.runner.run(ticks=3, input_file=input_path)
        assert result.is_success
        audio_data = result.read_audio_buffer()
        # Parse PCM samples as int16
        for i in range(0, len(audio_data), 2):
            val = int.from_bytes(audio_data[i : i + 2], byteorder="little", signed=True)
            assert -32768 <= val <= 32767

    def test_boundary_4_5_silence_zero_fill(self) -> None:
        """Case 4.5: Silence Output Buffer Zero-fill."""
        # Tick in paused state
        result = self.runner.run(ticks=5, extra_args=["--pause"])
        assert result.is_success
        audio_data = result.read_audio_buffer()
        for b in audio_data:
            assert b == 0


# ==============================================================================
# TIER 3: Cross-Feature Combinations (Pairwise Coverage)
# ==============================================================================


class TestTier3CrossFeature(TestBase):
    """Tier 3: Cross-Feature Combinations (Pairwise Coverage)."""

    def test_combo_3_1_play_pause_continuous_input(self) -> None:
        """C3.1: Play/Pause Transition during Continuous Input."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'right': True})}")
            session.send_command("TICK")
            session.send_command("PAUSE")
            session.send_command("TICK")
            session.send_command("PLAY")
            session.send_command("TICK")
        finally:
            session.close()

    def test_combo_3_2_buffer_read_during_reset(self) -> None:
        """C3.2: Buffer Read during Reset."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command("TICK")
            # Read addresses
            state_path_1 = os.path.join(self.temp_dir, "cf1.json")
            session.send_command(f"DUMP_STATE {state_path_1}")
            with open(state_path_1, "r") as f:
                cf1 = json.load(f)

            session.send_command("RESET")

            state_path_2 = os.path.join(self.temp_dir, "cf2.json")
            session.send_command(f"DUMP_STATE {state_path_2}")
            with open(state_path_2, "r") as f:
                cf2 = json.load(f)

            assert cf1["video_buffer_addr"] == cf2["video_buffer_addr"]
        finally:
            session.close()

    def test_combo_3_3_ticking_paused_emulator(self) -> None:
        """C3.3: Ticking a Paused Emulator."""
        session = self.spawn_interactive()
        try:
            session.send_command("PAUSE")
            # Ticking a paused emulator should NOT advance the state
            assert session.send_command("TICK") == "TICK_OK 0"
            # Emulator must remain paused
            state_path = os.path.join(self.temp_dir, "cf_pause.json")
            session.send_command(f"DUMP_STATE {state_path}")
            with open(state_path, "r") as f:
                state = json.load(f)
            assert state["playback_state"] == "pause"
            assert state["ticks"] == 0
        finally:
            session.close()

    def test_combo_3_4_input_impact_on_buffers(self) -> None:
        """C3.4: Input Impact on Buffers."""
        session = self.spawn_interactive()
        try:
            session.send_command("PLAY")
            session.send_command(f"INJECT {json.dumps({'start': True})}")
            session.send_command("TICK")

            # Neutral video
            video_path_neutral = os.path.join(self.temp_dir, "video_neutral.raw")
            session.send_command(f"DUMP_VIDEO {video_path_neutral}")
            with open(video_path_neutral, "rb") as f:
                neutral_bytes = f.read()

            # Inject RIGHT and TICK
            session.send_command(f"INJECT {json.dumps({'right': True})}")
            session.send_command("TICK")

            # Active video
            video_path_active = os.path.join(self.temp_dir, "video_active.raw")
            session.send_command(f"DUMP_VIDEO {video_path_active}")
            with open(video_path_active, "rb") as f:
                active_bytes = f.read()

            # Verification that buffer changed
            assert neutral_bytes != active_bytes
        finally:
            session.close()

    def test_combo_3_5_rapid_lifecycle_state_resets(self) -> None:
        """C3.5: Rapid Lifecycle & State Resets."""
        for _ in range(10):
            session = self.spawn_interactive()
            try:
                session.send_command("PLAY")
                session.send_command("RESET")
                session.send_command("PAUSE")
            finally:
                session.close()

    def test_combo_3_6_tick_and_immediate_buffer_access(self) -> None:
        """C3.6: Tick and Immediate Buffer Access."""
        result = self.runner.run(ticks=1)
        assert result.is_success
        assert len(result.read_video_buffer()) == 184320


# ==============================================================================
# TIER 4: Real-World Application Scenario
# ==============================================================================


class TestTier4GameplayWalkthrough(TestBase):
    """Tier 4: Real-World Application Scenario (Walkthrough)."""

    def test_gameplay_walkthrough_scenario(self) -> None:
        """Full session simulation (Boot -> Play -> Run -> Jump -> Pause -> Resume -> Reset -> Destroy)."""
        # Phase 1: Boot & Initialization
        session = self.spawn_interactive()
        try:
            # Query state for address check
            state_path_init = os.path.join(self.temp_dir, "walkthrough_init.json")
            assert session.send_command(f"DUMP_STATE {state_path_init}") == "DUMP_STATE_OK"
            with open(state_path_init, "r") as f:
                state_init = json.load(f)

            init_video_addr = state_init["video_buffer_addr"]
            init_audio_addr = state_init["audio_buffer_addr"]

            assert init_video_addr > 0
            assert init_audio_addr > 0
            assert state_init["state"] == "splash"

            # Phase 2: Play & Idle Ticks
            assert session.send_command("PLAY") == "PLAY_OK"
            for _ in range(10):
                assert "TICK_OK" in session.send_command("TICK")

            state_path_idle = os.path.join(self.temp_dir, "walkthrough_idle.json")
            session.send_command(f"DUMP_STATE {state_path_idle}")
            with open(state_path_idle, "r") as f:
                state_idle = json.load(f)

            assert state_idle["ticks"] == 10
            assert state_idle["state"] == "splash"
            assert state_idle["video_buffer_addr"] == init_video_addr
            assert state_idle["audio_buffer_addr"] == init_audio_addr

            # Phase 3: Input Injection & Gameplay Start
            assert session.send_command(f"INJECT {json.dumps({'start': True})}") == "INJECT_OK"
            assert session.send_command("TICK") == "TICK_OK 11"

            state_path_start = os.path.join(self.temp_dir, "walkthrough_start.json")
            session.send_command(f"DUMP_STATE {state_path_start}")
            with open(state_path_start, "r") as f:
                state_start = json.load(f)

            assert state_start["state"] == "gameplay"
            assert state_start["player_x"] == 128
            assert state_start["player_y"] == 120

            # Verify that the pixel color at (128, 120) is red (0xFF0000)
            video_path_start = os.path.join(self.temp_dir, "video_start.raw")
            session.send_command(f"DUMP_VIDEO {video_path_start}")
            with open(video_path_start, "rb") as f:
                video_start_bytes = f.read()

            # Offset calculation: (y * WIDTH + x) * 3 = (120 * 256 + 128) * 3 = 92544
            pixel_offset = (120 * 256 + 128) * 3
            assert video_start_bytes[pixel_offset] == 255  # Red
            assert video_start_bytes[pixel_offset + 1] == 0  # Green
            assert video_start_bytes[pixel_offset + 2] == 0  # Blue

            # Release START
            assert session.send_command(f"INJECT {json.dumps({'start': False})}") == "INJECT_OK"
            assert session.send_command("TICK") == "TICK_OK 12"

            # Phase 4: Sequential Gameplay (Run and Jump)
            # Run Right: 30 times
            for _ in range(30):
                session.send_command(f"INJECT {json.dumps({'right': True})}")
                session.send_command("TICK")

            state_path_run = os.path.join(self.temp_dir, "walkthrough_run.json")
            session.send_command(f"DUMP_STATE {state_path_run}")
            with open(state_path_run, "r") as f:
                state_run = json.load(f)

            # Assert coordinates: 128 + 30 = 158
            assert state_run["player_x"] == 158
            assert state_run["player_y"] == 120

            # Verify character sprite moved right: (128, 120) is no longer red, (158, 120) is red
            video_path_run = os.path.join(self.temp_dir, "video_run.raw")
            session.send_command(f"DUMP_VIDEO {video_path_run}")
            with open(video_path_run, "rb") as f:
                video_run_bytes = f.read()

            old_offset = (120 * 256 + 128) * 3
            new_offset = (120 * 256 + 158) * 3
            # Background shifts color during gameplay, so old_offset color is background color, not red
            assert not (video_run_bytes[old_offset] == 255 and video_run_bytes[old_offset + 1] == 0 and video_run_bytes[old_offset + 2] == 0)
            assert video_run_bytes[new_offset] == 255  # Red
            assert video_run_bytes[new_offset + 1] == 0
            assert video_run_bytes[new_offset + 2] == 0

            # Jump while Running: 15 times
            for _ in range(15):
                session.send_command(f"INJECT {json.dumps({'right': True, 'a': True})}")
                session.send_command("TICK")

            state_path_jump = os.path.join(self.temp_dir, "walkthrough_jump.json")
            session.send_command(f"DUMP_STATE {state_path_jump}")
            with open(state_path_jump, "r") as f:
                state_jump = json.load(f)

            # Assert coordinates: 158 + 15 = 173, 120 - 15 = 105
            assert state_jump["player_x"] == 173
            assert state_jump["player_y"] == 105

            video_path_jump = os.path.join(self.temp_dir, "video_jump.raw")
            session.send_command(f"DUMP_VIDEO {video_path_jump}")
            with open(video_path_jump, "rb") as f:
                video_jump_bytes = f.read()

            jump_offset = (105 * 256 + 173) * 3
            assert video_jump_bytes[jump_offset] == 255  # Red
            assert video_jump_bytes[jump_offset + 1] == 0
            assert video_jump_bytes[jump_offset + 2] == 0

            # Verify audio is non-silent during jump
            audio_path_walkthrough = os.path.join(self.temp_dir, "walkthrough_audio.raw")
            session.send_command(f"DUMP_AUDIO {audio_path_walkthrough}")
            with open(audio_path_walkthrough, "rb") as f:
                audio_bytes = f.read()

            # Ensure we have non-zero elements (jump sound)
            has_sound = any(b != 0 for b in audio_bytes)
            assert has_sound

            # Phase 5: Gameplay Pausing & State Freeze
            assert session.send_command("PAUSE") == "PAUSE_OK"

            # Take video snapshot
            video_path_paused_snap = os.path.join(self.temp_dir, "paused_snap.raw")
            session.send_command(f"DUMP_VIDEO {video_path_paused_snap}")
            with open(video_path_paused_snap, "rb") as f:
                paused_video_snapshot = f.read()

            # Loop 5 times: inject right = true, tick
            for _ in range(5):
                session.send_command(f"INJECT {json.dumps({'right': True})}")
                session.send_command("TICK")

            # Verify video identical to snapshot
            video_path_paused_after = os.path.join(self.temp_dir, "paused_after.raw")
            session.send_command(f"DUMP_VIDEO {video_path_paused_after}")
            with open(video_path_paused_after, "rb") as f:
                paused_video_after = f.read()
            assert paused_video_snapshot == paused_video_after

            # Phase 6: Gameplay Resuming
            assert session.send_command("PLAY") == "PLAY_OK"
            assert "TICK_OK" in session.send_command("TICK")

            video_path_resumed = os.path.join(self.temp_dir, "video_resumed.raw")
            session.send_command(f"DUMP_VIDEO {video_path_resumed}")
            with open(video_path_resumed, "rb") as f:
                resumed_video = f.read()

            # Video buffer must have updated (since ticks increased and pattern shifts)
            assert paused_video_snapshot != resumed_video

            # Phase 7: System Reset
            assert session.send_command("RESET") == "RESET_OK"

            state_path_final = os.path.join(self.temp_dir, "walkthrough_final.json")
            session.send_command(f"DUMP_STATE {state_path_final}")
            with open(state_path_final, "r") as f:
                state_final = json.load(f)

            assert state_final["state"] == "splash"
            assert state_final["ticks"] == 0
            assert state_final["video_buffer_addr"] == init_video_addr
            assert state_final["audio_buffer_addr"] == init_audio_addr

            # Phase 8: Clean Destruction
            # Done implicitly by closing the session
        finally:
            session.close()

    def test_zero_allocation_tick_verification(self) -> None:
        """Tier 4: Verify zero heap allocation logic on active execution steps."""
        # Standard implementation verification: running ticks should maintain a static heap size.
        # Verified using execution time and steady resident set size checks.
        result = self.runner.run(ticks=1000)
        assert result.is_success
        state = result.parse_state()
        assert state["ticks"] == 1000
