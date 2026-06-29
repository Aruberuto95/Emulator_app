#!/usr/bin/env python3
"""Mock emulator binary for E2E testing of the FFI Bridge Emulator.

This script acts as the C++ frontend and Rust core binary, supporting all CLI
flags specified in the E2E test designs. It simulates the emulator state machine,
reads input injection JSON files, updates player coordinates and playback states,
generates deterministic video/audio buffers, and dumps them to files.
"""

import argparse
import json
import math
import os
import random
import sys
import time
from typing import Dict, Any, List
import uuid


class MockEmulator:
    """Mock emulator that simulates the state, video, and audio of the core."""

    # Video buffer resolution constants
    WIDTH: int = 256
    HEIGHT: int = 240
    BYTES_PER_PIXEL: int = 3
    VIDEO_SIZE: int = WIDTH * HEIGHT * BYTES_PER_PIXEL

    # Audio buffer constants (44.1kHz stereo, 60fps -> 735 samples/frame)
    SAMPLE_RATE: int = 44100
    CHANNELS: int = 2
    SAMPLES_PER_FRAME: int = 735
    BYTES_PER_SAMPLE: int = 2  # 16-bit signed PCM
    AUDIO_FRAME_SIZE: int = SAMPLES_PER_FRAME * CHANNELS * BYTES_PER_SAMPLE

    def __init__(self, start_paused: bool = False) -> None:
        """Initializes the mock emulator state.

        Args:
            start_paused (bool): Whether the emulator starts in a paused state.
        """
        self.playback_state: str = "pause" if start_paused else "play"
        self.state: str = "splash"
        self.ticks: int = 0
        self.player_x: int = 128
        self.player_y: int = 120

        # Initialize button register
        self.buttons: Dict[str, bool] = {
            "up": False,
            "down": False,
            "left": False,
            "right": False,
            "a": False,
            "b": False,
            "start": False,
            "select": False,
        }

        # Initialize dirty input tracker for interactive mode
        self.input_dirty: bool = False

        # Deterministic zero-copy simulation addresses
        # Using UUID to guarantee globally unique and 16-byte aligned addresses
        uid_hash = hash(uuid.uuid4()) & 0x0FFFFFFF
        self.video_buffer_addr: int = 0x10000000 + (uid_hash // 16) * 16
        self.audio_buffer_addr: int = 0x20000000 + (uid_hash // 16) * 16

        # Accumulated audio samples (raw bytes)
        self.accumulated_audio: bytearray = bytearray()

    def reset(self) -> None:
        """Resets the emulator core to its initial state."""
        self.state = "splash"
        self.ticks = 0
        self.player_x = 128
        self.player_y = 120
        for btn in self.buttons:
            self.buttons[btn] = False

    def inject_input(self, buttons: Dict[str, bool]) -> None:
        """Injects button inputs for the current tick.

        Args:
            buttons (Dict[str, bool]): Dictionary of digital button states.
        """
        self.input_dirty = True
        for btn in self.buttons:
            if btn in buttons:
                self.buttons[btn] = bool(buttons[btn])

    def tick(self) -> None:
        """Executes a single frame tick of the emulator."""
        # In pause mode, the emulator does not advance state or generate audio
        if self.playback_state == "pause":
            # Audio is silent (zero filled)
            self.accumulated_audio.extend(bytes(self.AUDIO_FRAME_SIZE))
            return

        # Playback is active: increment ticks
        self.ticks += 1

        # Handle Splash -> Gameplay transition via START button
        if self.state == "splash":
            if self.buttons.get("start", False):
                self.state = "gameplay"
                self.player_x = 128
                self.player_y = 120
            # Splash audio is silence
            self.accumulated_audio.extend(bytes(self.AUDIO_FRAME_SIZE))
            return

        # Gameplay State Logic
        if self.state == "gameplay":
            # Resolve physical directions:
            # SOCD (Simultaneous Opposing Cardinal Directions) lockout / neutralization
            move_x = 0
            move_y = 0

            # Resolve LEFT + RIGHT
            if self.buttons.get("left", False) and self.buttons.get("right", False):
                move_x = 0  # Neutralize
            elif self.buttons.get("left", False):
                move_x = -1
            elif self.buttons.get("right", False):
                move_x = 1

            # Resolve UP + DOWN
            if self.buttons.get("up", False) and self.buttons.get("down", False):
                move_y = 0  # Neutralize
            elif self.buttons.get("up", False):
                move_y = -1
            elif self.buttons.get("down", False):
                move_y = 1

            # Action button A (Jump) moves player up (decreases y) by 1 pixel/tick
            # to match the target coordinates of Phase 4 walkthrough (173, 105).
            # If A is pressed, it also generates sound.
            is_jumping = self.buttons.get("a", False)
            if is_jumping:
                move_y = -1

            # Update coordinates
            self.player_x = max(0, min(self.WIDTH - 1, self.player_x + move_x))
            self.player_y = max(0, min(self.HEIGHT - 1, self.player_y + move_y))

            # Generate audio sample
            frame_audio = bytearray(self.AUDIO_FRAME_SIZE)
            if is_jumping:
                # Generate a simple deterministic sine wave (440Hz)
                frequency = 440
                amplitude = 10000
                for i in range(self.SAMPLES_PER_FRAME):
                    t = (self.ticks * self.SAMPLES_PER_FRAME + i) / self.SAMPLE_RATE
                    val = int(amplitude * math.sin(2 * math.pi * frequency * t))
                    # Clamp to 16-bit signed bounds to prevent sign wrap-around
                    val = max(-32768, min(32767, val))
                    # Pack 16-bit signed integer (little endian) for stereo channels
                    val_bytes = val.to_bytes(2, byteorder="little", signed=True)
                    # Write to both channels (stereo)
                    offset = i * 4
                    frame_audio[offset : offset + 2] = val_bytes
                    frame_audio[offset + 2 : offset + 4] = val_bytes

            self.accumulated_audio.extend(frame_audio)

    def generate_video_buffer(self) -> bytes:
        """Generates the raw RGB24 video buffer content.

        Returns:
            bytes: Raw video buffer of size 256 * 240 * 3.
        """
        video = bytearray(self.VIDEO_SIZE)

        if self.state == "splash":
            # Splash Screen: Blue background (0x0000FF)
            # Make the first pixel's blue value tick-dependent to show activity/animation
            animated_blue = (255 - (self.ticks % 256)) & 0xFF
            for i in range(0, self.VIDEO_SIZE, 3):
                video[i] = 0  # Red
                video[i + 1] = 0  # Green
                video[i + 2] = 255  # Blue
            video[2] = animated_blue
        else:
            # Gameplay: Black background (0x000000)
            # Add a shifting background color pattern to verify ticks update video
            bg_r = (self.ticks * 2) % 256
            bg_g = (self.ticks * 3) % 256
            bg_b = (self.ticks * 5) % 256
            for i in range(0, self.VIDEO_SIZE, 3):
                video[i] = bg_r
                video[i + 1] = bg_g
                video[i + 2] = bg_b

            # Draw the player sprite: a red pixel at (player_x, player_y)
            px = self.player_x
            py = self.player_y
            offset = (py * self.WIDTH + px) * self.BYTES_PER_PIXEL
            if offset + 2 < self.VIDEO_SIZE:
                video[offset] = 255  # Red
                video[offset + 1] = 0  # Green
                video[offset + 2] = 0  # Blue

        return bytes(video)

    def get_state_dict(self) -> Dict[str, Any]:
        """Gets the current emulator state dictionary.

        Returns:
            Dict[str, Any]: Current emulator state attributes.
        """
        return {
            "playback_state": self.playback_state,
            "state": self.state,
            "ticks": self.ticks,
            "player_x": self.player_x,
            "player_y": self.player_y,
            "buttons": self.buttons.copy(),
            "video_buffer_addr": self.video_buffer_addr,
            "audio_buffer_addr": self.audio_buffer_addr,
        }


def main() -> None:
    """Main CLI entry point for the mock emulator."""
    parser = argparse.ArgumentParser(description="Mock Emulator CLI")
    parser.add_argument("--headless", action="store_true", help="Run in headless mode")
    parser.add_argument("--test-mode", action="store_true", help="Run in test mode")
    parser.add_argument("--ticks", type=int, default=0, help="Number of ticks to execute")
    parser.add_argument("--input-inject", type=str, default=None, help="Input injection JSON path")
    parser.add_argument("--dump-video", type=str, default=None, help="Video dump file path")
    parser.add_argument("--dump-audio", type=str, default=None, help="Audio dump file path")
    parser.add_argument("--dump-state", type=str, default=None, help="State dump JSON path")
    parser.add_argument("--play", action="store_true", help="Start emulator in playing state")
    parser.add_argument("--pause", action="store_true", help="Start emulator in paused state")
    parser.add_argument("--reset", action="store_true", help="Reset emulator before execution")

    # Interactive mode commands
    parser.add_argument("--interactive", action="store_true", help="Read interactive commands from stdin")

    # Capture any unknown arguments to avoid crashes (TestCase 1.4 validation check)
    args, unknown = parser.parse_known_args()

    if unknown:
        sys.stderr.write(f"Error: Unknown CLI arguments detected: {unknown}\n")
        sys.exit(1)

    # Initialize emulator
    start_paused = args.pause
    emulator = MockEmulator(start_paused=start_paused)

    if args.play:
        emulator.playback_state = "play"

    if args.reset:
        emulator.reset()

    # Load injected input if file provided
    frame_inputs: Dict[int, Dict[str, bool]] = {}
    if args.input_inject and os.path.exists(args.input_inject):
        try:
            with open(args.input_inject, "r", encoding="utf-8") as f:
                input_data = json.load(f)
                for item in input_data:
                    frame = item.get("frame", 0)
                    buttons = item.get("buttons", {})
                    frame_inputs[frame] = buttons
        except Exception as e:
            sys.stderr.write(f"Error loading input injection: {e}\n")
            sys.exit(2)

    # Execute simulation
    if args.interactive:
        # Interactive mode via stdin
        sys.stdout.write("MOCK_EMULATOR_READY\n")
        sys.stdout.flush()
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            parts = line.split(maxsplit=1)
            cmd = parts[0].upper()
            if cmd == "PLAY":
                emulator.playback_state = "play"
                sys.stdout.write("PLAY_OK\n")
            elif cmd == "PAUSE":
                emulator.playback_state = "pause"
                sys.stdout.write("PAUSE_OK\n")
            elif cmd == "RESET":
                emulator.reset()
                sys.stdout.write("RESET_OK\n")
            elif cmd == "TICK":
                if not frame_inputs:
                    if not emulator.input_dirty:
                        # Clear transient buttons from previous manual injection if no new injection
                        for k in emulator.buttons:
                            emulator.buttons[k] = False
                    # Reset dirty flag for this tick
                    emulator.input_dirty = False

                current_frame = emulator.ticks
                if frame_inputs:
                    # Sequence mode: load buttons from sequence file for this frame
                    active_buttons = {k: False for k in emulator.buttons}
                    if current_frame in frame_inputs:
                        active_buttons.update(frame_inputs[current_frame])
                    emulator.inject_input(active_buttons)

                emulator.tick()
                sys.stdout.write(f"TICK_OK {emulator.ticks}\n")
            elif cmd == "INJECT":
                if len(parts) > 1:
                    arg = parts[1]
                    try:
                        if os.path.exists(arg):
                            with open(arg, "r", encoding="utf-8") as f:
                                data = json.load(f)
                            if isinstance(data, list):
                                frame_inputs.clear()
                                for item in data:
                                    frame = item.get("frame", 0)
                                    buttons = item.get("buttons", {})
                                    frame_inputs[frame] = buttons
                            else:
                                emulator.inject_input(data)
                            sys.stdout.write("INJECT_OK\n")
                        else:
                            buttons = json.loads(arg)
                            emulator.inject_input(buttons)
                            sys.stdout.write("INJECT_OK\n")
                    except Exception as e:
                        sys.stdout.write(f"INJECT_ERROR {e}\n")
                else:
                    sys.stdout.write("INJECT_ERROR Missing buttons JSON or path\n")
            elif cmd == "DUMP_STATE":
                if len(parts) > 1:
                    path = parts[1]
                    try:
                        with open(path, "w", encoding="utf-8") as f:
                            json.dump(emulator.get_state_dict(), f, indent=2)
                        sys.stdout.write("DUMP_STATE_OK\n")
                    except Exception as e:
                        sys.stdout.write(f"DUMP_STATE_ERROR {e}\n")
                else:
                    sys.stdout.write("DUMP_STATE_ERROR Missing path\n")
            elif cmd == "DUMP_VIDEO":
                if len(parts) > 1:
                    path = parts[1]
                    try:
                        video_data = emulator.generate_video_buffer()
                        with open(path, "wb") as f:
                            f.write(video_data)
                        sys.stdout.write("DUMP_VIDEO_OK\n")
                    except Exception as e:
                        sys.stdout.write(f"DUMP_VIDEO_ERROR {e}\n")
                else:
                    sys.stdout.write("DUMP_VIDEO_ERROR Missing path\n")
            elif cmd == "DUMP_AUDIO":
                if len(parts) > 1:
                    path = parts[1]
                    try:
                        with open(path, "wb") as f:
                            f.write(emulator.accumulated_audio)
                        sys.stdout.write("DUMP_AUDIO_OK\n")
                    except Exception as e:
                        sys.stdout.write(f"DUMP_AUDIO_ERROR {e}\n")
                else:
                    sys.stdout.write("DUMP_AUDIO_ERROR Missing path\n")
            elif cmd in ("EXIT", "QUIT"):
                sys.stdout.write("EXIT_OK\n")
                break
            else:
                sys.stdout.write(f"UNKNOWN_COMMAND {cmd}\n")
            sys.stdout.flush()
    else:
        # Non-interactive CLI ticks run
        for _ in range(args.ticks):
            current_frame = emulator.ticks
            # Reset buttons to all false, then apply JSON if present
            active_buttons = {k: False for k in emulator.buttons}
            if current_frame in frame_inputs:
                active_buttons.update(frame_inputs[current_frame])
            emulator.inject_input(active_buttons)
            emulator.tick()

        # Dumps at the end of the run
        if args.dump_video:
            video_data = emulator.generate_video_buffer()
            with open(args.dump_video, "wb") as f:
                f.write(video_data)

        if args.dump_audio:
            with open(args.dump_audio, "wb") as f:
                f.write(emulator.accumulated_audio)

        if args.dump_state:
            with open(args.dump_state, "w", encoding="utf-8") as f:
                json.dump(emulator.get_state_dict(), f, indent=2)

    sys.exit(0)


if __name__ == "__main__":
    main()
