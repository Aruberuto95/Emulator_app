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
import sys
from typing import Dict, Any
import uuid

# Standard GBC and GBA Nintendo Logo bytes for header verification
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


def check_path_safety(path: str, base_dir: str) -> bool:
    """Helper to check that a path resides within the allowed base directory.

    Args:
        path (str): The path to check.
        base_dir (str): The allowed base directory.

    Returns:
        bool: True if safe, False if traversal is detected.
    """
    try:
        abs_path = os.path.abspath(path)
        abs_base = os.path.abspath(base_dir)
        real_path = os.path.realpath(path)
        real_base = os.path.realpath(base_dir)
        
        # Check traversal in unresolved path
        if not abs_path.startswith(abs_base):
            return False
        # Check traversal in resolved path (resolving symlinks)
        if not real_path.startswith(real_base):
            return False
        return True
    except Exception:
        return False


class MockEmulator:
    """Mock emulator that simulates the state, video, and audio of GBC and GBA cores."""

    BYTES_PER_PIXEL: int = 3

    # Audio buffer constants (44.1kHz stereo, 60fps -> 735 samples/frame)
    SAMPLE_RATE: int = 44100
    CHANNELS: int = 2
    BYTES_PER_SAMPLE: int = 2  # 16-bit signed PCM

    def __init__(self, start_paused: bool = False) -> None:
        """Initializes the mock emulator state.

        Args:
            start_paused (bool): Whether the emulator starts in a paused state.
        """
        self.playback_state: str = "pause" if start_paused else "play"
        self.state: str = "splash"
        self.ticks: int = 0

        self.console_type: str = "GBC"
        self.width: int = 160
        self.height: int = 144
        self.video_size: int = self.width * self.height * self.BYTES_PER_PIXEL

        self.player_x: int = 80
        self.player_y: int = 72

        self.speed: float = 1.0
        self.frame_skip: int = 0
        self.cpu_cycles: int = 0
        self.rendered_frames: int = 0

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
            "l": False,
            "r": False,
        }

        # Initialize dirty input tracker for interactive mode
        self.input_dirty: bool = False

        # Deterministic zero-copy simulation addresses
        # Using UUID to guarantee globally unique and 16-byte aligned addresses
        uid_hash = hash(uuid.uuid4()) & 0x0FFFFFFF
        self.video_buffer_addr: int = 0x10000000 + (uid_hash // 16) * 16
        self.audio_buffer_addr: int = 0x20000000 + (uid_hash // 16) * 16

        # Cached video buffer for frame skipping
        self.cached_video_buffer: bytes = b""

        # Accumulated audio samples (raw bytes)
        self.accumulated_audio: bytearray = bytearray()
        self.extra_fields: Dict[str, Any] = {}


    def update_console_dimensions(self) -> None:
        """Updates emulator screen dimensions and video buffer size based on console type."""
        if self.console_type == "GBA":
            self.width = 240
            self.height = 160
        else:
            self.width = 160
            self.height = 144
        self.video_size = self.width * self.height * self.BYTES_PER_PIXEL
        # Recenter player
        if self.console_type == "GBA":
            self.player_x = 120
            self.player_y = 80
        else:
            self.player_x = 80
            self.player_y = 72
        # Reset cached video buffer
        self.cached_video_buffer = b""

    def reset_on_rom_load(self) -> None:
        """Resets ticks, cycles, and audio buffer on ROM load."""
        self.ticks = 0
        self.cpu_cycles = 0
        self.rendered_frames = 0
        self.accumulated_audio = bytearray()
        self.cached_video_buffer = b""

    def reset(self) -> None:
        """Resets the emulator core to its initial state."""
        self.state = "splash"
        self.ticks = 0
        self.console_type = "GBC"
        self.update_console_dimensions()
        self.speed = 1.0
        self.frame_skip = 0
        self.cpu_cycles = 0
        self.rendered_frames = 0
        self.accumulated_audio = bytearray()
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

    def set_speed(self, speed_str: str) -> str:
        """Sets emulator execution speed.

        Args:
            speed_str (str): Target speed as float.

        Returns:
            str: "OK" or error message.
        """
        try:
            val = float(speed_str)
            if val <= 0:
                return "SET_SPEED_ERROR Speed must be positive"
            if val > 1000.0:
                return "SET_SPEED_ERROR Speed exceeds maximum limit"
            self.speed = val
            return "OK"
        except ValueError:
            return "SET_SPEED_ERROR Non-numeric speed"

    def set_frame_skip(self, skip_str: str) -> str:
        """Sets frame skipping count.

        Args:
            skip_str (str): Skip count as integer.

        Returns:
            str: "OK" or error message.
        """
        try:
            val = int(skip_str)
            if val < 0:
                return "SET_FRAME_SKIP_ERROR Frame skip cannot be negative"
            if val > 1000:
                return "SET_FRAME_SKIP_ERROR Frame skip exceeds maximum limit"
            self.frame_skip = val
            return "OK"
        except ValueError:
            return "SET_FRAME_SKIP_ERROR Non-integer frame skip"

    def tick(self) -> None:
        """Executes a single frame tick of the emulator."""
        # Audio increment even when paused or skipped
        audio_frame_size = int(735 * self.speed) * self.CHANNELS * self.BYTES_PER_SAMPLE

        # In pause mode, the emulator does not advance state or generate audio (returns silence)
        if self.playback_state == "pause":
            self.accumulated_audio.extend(bytes(audio_frame_size))
            return

        # Playback is active: increment ticks
        self.ticks += 1
        
        # Determine if we should render this tick
        is_render_tick = (self.ticks % (self.frame_skip + 1) == 0)

        # Handle Splash -> Gameplay transition via START button
        if self.state == "splash":
            if self.buttons.get("start", False):
                self.state = "gameplay"
                if self.console_type == "GBA":
                    self.player_x = 120
                    self.player_y = 80
                else:
                    self.player_x = 80
                    self.player_y = 72
            # Splash audio is silence
            self.accumulated_audio.extend(bytes(audio_frame_size))
            # CPU cycles
            if self.console_type == "GBA":
                self.cpu_cycles = (self.cpu_cycles + int(280896 * self.speed)) & 0xFFFFFFFFFFFFFFFF
            else:
                self.cpu_cycles = (self.cpu_cycles + int(70224 * self.speed)) & 0xFFFFFFFFFFFFFFFF

            if is_render_tick:
                self.cached_video_buffer = self._render_video()
                self.rendered_frames += 1
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
            is_jumping = self.buttons.get("a", False)
            if is_jumping:
                move_y = -1

            # Update coordinates with boundary checks
            self.player_x = max(0, min(self.width - 1, self.player_x + move_x))
            self.player_y = max(0, min(self.height - 1, self.player_y + move_y))

            # CPU cycles
            if self.console_type == "GBA":
                self.cpu_cycles = (self.cpu_cycles + int(280896 * self.speed)) & 0xFFFFFFFFFFFFFFFF
            else:
                self.cpu_cycles = (self.cpu_cycles + int(70224 * self.speed)) & 0xFFFFFFFFFFFFFFFF

            # Generate audio sample
            num_samples = int(735 * self.speed)
            frame_audio = bytearray(num_samples * 4)
            if is_jumping:
                # Sine wave (440Hz)
                frequency = 440
                amplitude = 10000
                for i in range(num_samples):
                    t = (self.ticks * 735 + i) / self.SAMPLE_RATE
                    val = int(amplitude * math.sin(2 * math.pi * frequency * t))
                    # Clamp to 16-bit signed bounds to prevent sign wrap-around
                    val = max(-32768, min(32767, val))
                    # Pack 16-bit signed integer (little endian) for stereo channels
                    val_bytes = val.to_bytes(2, byteorder="little", signed=True)
                    offset = i * 4
                    frame_audio[offset : offset + 2] = val_bytes
                    frame_audio[offset + 2 : offset + 4] = val_bytes

            self.accumulated_audio.extend(frame_audio)

            if is_render_tick:
                self.cached_video_buffer = self._render_video()
                self.rendered_frames += 1

    def _render_video(self) -> bytes:
        """Helper to render video buffer based on current state."""
        if self.state == "splash":
            # Splash Screen: Blue background
            animated_blue = (255 - (self.ticks % 256)) & 0xFF
            pixel = bytes([0, 0, 255])
            video = bytearray(pixel * (self.video_size // 3))
            video[2] = animated_blue
        else:
            # Gameplay: background pattern
            bg_r = (self.ticks * 2) % 256
            bg_g = (self.ticks * 3) % 256
            bg_b = (self.ticks * 5) % 256
            pixel = bytes([bg_r, bg_g, bg_b])
            video = bytearray(pixel * (self.video_size // 3))

            # Draw the player sprite: a red pixel at (player_x, player_y)
            px = self.player_x
            py = self.player_y
            offset = (py * self.width + px) * self.BYTES_PER_PIXEL
            if offset + 2 < self.video_size:
                video[offset] = 255
                video[offset + 1] = 0
                video[offset + 2] = 0

        return bytes(video)

    def generate_video_buffer(self) -> bytes:
        """Returns the current video buffer.

        Returns:
            bytes: Raw video buffer.
        """
        if not self.cached_video_buffer:
            self.cached_video_buffer = self._render_video()
        return self.cached_video_buffer

    def load_rom(self, rom_path: str) -> str:
        """Loads a ROM, verifies its path safety and header.

        Args:
            rom_path (str): Path to the ROM file.

        Returns:
            str: "OK" if success, else error message starting with "LOAD_ROM_ERROR".
        """
        base_dir = "/Users/a.rudolph/Proyectos Albert/clothing_app"
        try:
            if not check_path_safety(rom_path, base_dir):
                return "LOAD_ROM_ERROR Path traversal detected"
        except Exception as e:
            return f"LOAD_ROM_ERROR Path safety check error: {e}"

        if not os.path.exists(rom_path):
            return "LOAD_ROM_ERROR File not found"

        # Check symlink pointing outside base_dir
        if os.path.islink(rom_path):
            try:
                target_path = os.readlink(rom_path)
                link_dir = os.path.dirname(rom_path)
                abs_target = os.path.abspath(os.path.join(link_dir, target_path))
                if not check_path_safety(abs_target, base_dir):
                    return "LOAD_ROM_ERROR Symlink points outside allowed directory"
            except Exception as e:
                return f"LOAD_ROM_ERROR Symlink resolution error: {e}"

        # Read ROM content
        try:
            with open(rom_path, "rb") as f:
                header_data = f.read(512)
        except Exception as e:
            return f"LOAD_ROM_ERROR Failed to read file: {e}"

        if len(header_data) == 0:
            return "LOAD_ROM_ERROR Empty ROM file"

        # Check if it is a text-based mock profile first
        is_text = False
        try:
            text_content = header_data.decode("utf-8", errors="strict")
            if "CONSOLE:" in text_content:
                is_text = True
        except UnicodeDecodeError:
            pass

        if is_text:
            lines = [line.strip() for line in text_content.splitlines()]
            console_type = None
            logo_valid = False
            checksum_valid = False
            for line in lines:
                if line.startswith("CONSOLE:"):
                    console_type = line.split(":", 1)[1].strip()
                elif line.startswith("LOGO:"):
                    logo_valid = (line.split(":", 1)[1].strip() == "VALID")
                elif line.startswith("CHECKSUM:"):
                    checksum_valid = (line.split(":", 1)[1].strip() == "VALID")
            
            if not console_type:
                return "LOAD_ROM_ERROR Missing CONSOLE type in text mock"
            if not logo_valid:
                return "LOAD_ROM_ERROR Invalid Nintendo logo"
            if not checksum_valid:
                return "LOAD_ROM_ERROR Invalid checksum"

            self.console_type = console_type
            self.update_console_dimensions()
            self.reset_on_rom_load()
            return "OK"

        # Binary ROM Header Parsing
        ext = os.path.splitext(rom_path)[1].lower()
        if ext == ".gba" or (len(header_data) >= 0xBD and header_data[0xB2] == 0x96):
            # GBA ROM
            if len(header_data) < 0xC0:
                return "LOAD_ROM_ERROR Truncated ROM"
            
            # GBA Logo (156 bytes at 0x004)
            gba_logo_bytes = header_data[0x004 : 0x004 + 156]
            if gba_logo_bytes != GBA_NINTENDO_LOGO:
                return "LOAD_ROM_ERROR Invalid Nintendo logo"
            
            # GBA Console Byte (0xB2) should be 0x96
            if header_data[0xB2] != 0x96:
                return "LOAD_ROM_ERROR GBA console byte mismatch"
                
            # Check GBA header checksum at 0xBD (covers 0xA0 to 0xBC)
            checksum = 0
            for i in range(0xA0, 0xBD):
                checksum = (checksum - header_data[i]) & 0xFF
            checksum = (checksum - 0x19) & 0xFF
            if header_data[0xBD] != checksum:
                return "LOAD_ROM_ERROR GBA header checksum mismatch"

            self.console_type = "GBA"
            self.update_console_dimensions()
            self.reset_on_rom_load()
            return "OK"
        else:
            # GBC ROM
            if len(header_data) < 0x150:
                return "LOAD_ROM_ERROR Truncated ROM"
            
            # GBC Logo (48 bytes at 0x104)
            gbc_logo_bytes = header_data[0x104 : 0x104 + 48]
            if gbc_logo_bytes != GBC_NINTENDO_LOGO:
                return "LOAD_ROM_ERROR Invalid Nintendo logo"

            # GBC Console Byte (0x143) should be 0x80 or 0xC0
            if header_data[0x143] not in (0x80, 0xC0):
                return "LOAD_ROM_ERROR GBC console byte mismatch"

            # Check GBC complement checksum at 0x14D (covers 0x134 to 0x14C)
            checksum = 0
            for i in range(0x134, 0x14D):
                checksum = (checksum - header_data[i] - 1) & 0xFF
            if header_data[0x14D] != checksum:
                return "LOAD_ROM_ERROR GBC header checksum mismatch"

            self.console_type = "GBC"
            self.update_console_dimensions()
            self.reset_on_rom_load()
            return "OK"

    def scan_roms(self, dir_path: str) -> str:
        """Scans a directory for valid GBC and GBA ROMs.

        Args:
            dir_path (str): Path to the directory to scan.

        Returns:
            str: JSON response starting with "SCAN_ROMS_OK" or error message.
        """
        base_dir = "/Users/a.rudolph/Proyectos Albert/clothing_app"
        try:
            if not check_path_safety(dir_path, base_dir):
                return "SCAN_ROMS_ERROR Path traversal detected"
        except Exception as e:
            return f"SCAN_ROMS_ERROR Path safety check error: {e}"

        if not os.path.exists(dir_path):
            return "SCAN_ROMS_ERROR Directory not found"
        if not os.path.isdir(dir_path):
            return "SCAN_ROMS_ERROR Not a directory"

        valid_roms = []
        try:
            for root, _, files in os.walk(dir_path):
                if not check_path_safety(root, base_dir):
                    continue
                for file in files:
                    ext = os.path.splitext(file)[1].lower()
                    if ext in (".gbc", ".gba"):
                        full_path = os.path.join(root, file)
                        if not check_path_safety(full_path, base_dir):
                            continue
                        
                        is_valid = self._validate_rom_file(full_path)
                        if is_valid:
                            console_type = "GBA" if ext == ".gba" else "GBC"
                            valid_roms.append({
                                "path": full_path,
                                "console_type": console_type
                            })
        except Exception as e:
            return f"SCAN_ROMS_ERROR Scan failed: {e}"

        return f"SCAN_ROMS_OK {json.dumps(valid_roms)}"

    def _validate_rom_file(self, rom_path: str) -> bool:
        """Helper to validate ROM file header without changing state."""
        try:
            with open(rom_path, "rb") as f:
                header_data = f.read(512)
        except Exception:
            return False

        if len(header_data) == 0:
            return False

        is_text = False
        try:
            text_content = header_data.decode("utf-8", errors="strict")
            if "CONSOLE:" in text_content:
                is_text = True
        except UnicodeDecodeError:
            pass

        if is_text:
            lines = [line.strip() for line in text_content.splitlines()]
            console_type = None
            logo_valid = False
            checksum_valid = False
            for line in lines:
                if line.startswith("CONSOLE:"):
                    console_type = line.split(":", 1)[1].strip()
                elif line.startswith("LOGO:"):
                    logo_valid = (line.split(":", 1)[1].strip() == "VALID")
                elif line.startswith("CHECKSUM:"):
                    checksum_valid = (line.split(":", 1)[1].strip() == "VALID")
            return bool(console_type and logo_valid and checksum_valid)

        ext = os.path.splitext(rom_path)[1].lower()
        if ext == ".gba":
            if len(header_data) < 0xC0:
                return False
            if header_data[0x004 : 0x004 + 156] != GBA_NINTENDO_LOGO:
                return False
            if header_data[0xB2] != 0x96:
                return False
            checksum = 0
            for i in range(0xA0, 0xBD):
                checksum = (checksum - header_data[i]) & 0xFF
            checksum = (checksum - 0x19) & 0xFF
            return header_data[0xBD] == checksum
        else:
            if len(header_data) < 0x150:
                return False
            if header_data[0x104 : 0x104 + 48] != GBC_NINTENDO_LOGO:
                return False
            if header_data[0x143] not in (0x80, 0xC0):
                return False
            checksum = 0
            for i in range(0x134, 0x14D):
                checksum = (checksum - header_data[i] - 1) & 0xFF
            return header_data[0x14D] == checksum

    def save_state(self, slot: str) -> str:
        """Saves state atomically.

        Args:
            slot (str): State slot.

        Returns:
            str: "OK" or error message.
        """
        base_dir = "/Users/a.rudolph/Proyectos Albert/clothing_app"
        dump_dir = os.environ.get("ALLOWED_DUMP_DIR", ".")
        
        tmp_filename = f"savestate_{slot}.tmp"
        sav_filename = f"savestate_{slot}.sav"
        
        if ".." in slot or "/" in slot or "\\" in slot:
            return "SAVE_STATE_ERROR Path traversal detected"

        tmp_path = os.path.join(dump_dir, tmp_filename)
        sav_path = os.path.join(dump_dir, sav_filename)

        try:
            if not check_path_safety(tmp_path, base_dir) or not check_path_safety(sav_path, base_dir):
                return "SAVE_STATE_ERROR Path traversal detected"
        except Exception as e:
            return f"SAVE_STATE_ERROR Path safety check error: {e}"

        # Mock simulated full disk write failure
        if os.environ.get("MOCK_DISK_FULL") == "1":
            return "SAVE_STATE_ERROR Disk full"

        try:
            os.makedirs(os.path.dirname(os.path.abspath(tmp_path)), exist_ok=True)
            
            state_data = {
                "console_type": self.console_type,
                "playback_state": self.playback_state,
                "ticks": self.ticks,
                "player_x": self.player_x,
                "player_y": self.player_y,
                "buttons": self.buttons,
                "speed": self.speed,
                "frame_skip": self.frame_skip,
                "cpu_cycles": self.cpu_cycles,
                "rendered_frames": self.rendered_frames,
            }
            state_data.update(self.extra_fields)
            # Write to tmp file
            with open(tmp_path, "w", encoding="utf-8") as f:
                json.dump(state_data, f, indent=2)
                f.flush()
                os.fsync(f.fileno())
            
            # Atomically replace
            os.replace(tmp_path, sav_path)
            return "OK"
        except Exception as e:
            if os.path.exists(tmp_path):
                try:
                    os.remove(tmp_path)
                except Exception:
                    pass
            return f"SAVE_STATE_ERROR {e}"

    def load_state(self, slot: str) -> str:
        """Loads state, rolling back on failure.

        Args:
            slot (str): State slot.

        Returns:
            str: "OK" or error message.
        """
        base_dir = "/Users/a.rudolph/Proyectos Albert/clothing_app"
        dump_dir = os.environ.get("ALLOWED_DUMP_DIR", ".")
        if ".." in slot or "/" in slot or "\\" in slot:
            return "LOAD_STATE_ERROR Path traversal detected"

        sav_path = os.path.join(dump_dir, f"savestate_{slot}.sav")

        try:
            if not check_path_safety(sav_path, base_dir):
                return "LOAD_STATE_ERROR Path traversal detected"
        except Exception as e:
            return f"LOAD_STATE_ERROR Path safety check error: {e}"

        if not os.path.exists(sav_path):
            return "LOAD_STATE_ERROR File not found"

        backup_state = {
            "console_type": self.console_type,
            "playback_state": self.playback_state,
            "ticks": self.ticks,
            "player_x": self.player_x,
            "player_y": self.player_y,
            "buttons": self.buttons.copy(),
            "speed": self.speed,
            "frame_skip": self.frame_skip,
            "cpu_cycles": self.cpu_cycles,
            "rendered_frames": self.rendered_frames,
        }

        try:
            with open(sav_path, "rb") as f:
                content_bytes = f.read(2 * 1024 * 1024 + 1)
            if len(content_bytes) > 2 * 1024 * 1024:
                return "LOAD_STATE_ERROR State file too large"
            state_data = json.loads(content_bytes.decode("utf-8"))
            
            required_keys = ["console_type", "playback_state", "ticks", "player_x", "player_y", "buttons", "speed", "frame_skip", "cpu_cycles", "rendered_frames"]
            for key in required_keys:
                if key not in state_data:
                    raise KeyError(f"Missing key in state data: {key}")

            self.console_type = state_data["console_type"]
            self.playback_state = state_data["playback_state"]
            self.ticks = state_data["ticks"]
            self.player_x = state_data["player_x"]
            self.player_y = state_data["player_y"]
            
            if self.console_type not in ("GBC", "GBA"):
                raise ValueError(f"Invalid console type: {self.console_type}")

            if self.console_type == "GBA":
                self.width = 240
                self.height = 160
            else:
                self.width = 160
                self.height = 144
            self.video_size = self.width * self.height * self.BYTES_PER_PIXEL
            self.cached_video_buffer = b""

            self.buttons = state_data["buttons"]
            self.speed = float(state_data["speed"])
            self.frame_skip = int(state_data["frame_skip"])
            self.cpu_cycles = int(state_data["cpu_cycles"])
            self.rendered_frames = int(state_data["rendered_frames"])
            self.extra_fields = {k: v for k, v in state_data.items() if k not in required_keys}
            return "OK"
        except Exception as e:
            # Rollback
            self.console_type = backup_state["console_type"]
            self.playback_state = backup_state["playback_state"]
            self.ticks = backup_state["ticks"]
            self.player_x = backup_state["player_x"]
            self.player_y = backup_state["player_y"]
            self.buttons = backup_state["buttons"]
            self.speed = backup_state["speed"]
            self.frame_skip = backup_state["frame_skip"]
            self.cpu_cycles = backup_state["cpu_cycles"]
            self.rendered_frames = backup_state["rendered_frames"]
            
            if self.console_type == "GBA":
                self.width = 240
                self.height = 160
            else:
                self.width = 160
                self.height = 144
            self.video_size = self.width * self.height * self.BYTES_PER_PIXEL
            self.cached_video_buffer = b""

            return f"LOAD_STATE_ERROR {e}"

    def get_state_dict(self) -> Dict[str, Any]:
        """Gets the current emulator state dictionary.

        Returns:
            Dict[str, Any]: Current emulator state attributes.
        """
        d = {
            "playback_state": self.playback_state,
            "state": self.state,
            "ticks": self.ticks,
            "console_type": self.console_type,
            "player_x": self.player_x,
            "player_y": self.player_y,
            "speed": self.speed,
            "frame_skip": self.frame_skip,
            "cpu_cycles": self.cpu_cycles,
            "rendered_frames": self.rendered_frames,
            "buttons": self.buttons.copy(),
            "video_buffer_addr": self.video_buffer_addr,
            "audio_buffer_addr": self.audio_buffer_addr,
        }
        d.update(self.extra_fields)
        return d


def main() -> None:
    """Main CLI entry point for the mock emulator."""
    # Pre-parse validation for --ticks to match test_adversarial requirements
    ticks_val = None
    for idx, arg in enumerate(sys.argv):
        if arg == "--ticks":
            if idx + 1 < len(sys.argv):
                ticks_val = sys.argv[idx + 1]
                break

    if ticks_val is not None:
        try:
            val = int(ticks_val)
            if val < 0:
                sys.stderr.write("Error: --ticks cannot be negative\n")
                sys.exit(1)
            if abs(val) > 2**63 - 1:
                sys.stderr.write("Error: Invalid ticks value\n")
                sys.exit(1)
        except ValueError:
            sys.stderr.write("Error: Invalid ticks value\n")
            sys.exit(1)

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
    
    # New options
    parser.add_argument("--rom", type=str, default=None, help="ROM path to load")
    parser.add_argument("--speed", type=float, default=None, help="Playback speed multiplier")
    parser.add_argument("--frame-skip", type=int, default=None, help="Frame skip count")

    # Interactive mode commands
    parser.add_argument("--interactive", action="store_true", help="Read interactive commands from stdin")

    # Capture any unknown arguments to avoid crashes
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

    # Apply other CLI configurations
    if args.speed is not None:
        res = emulator.set_speed(str(args.speed))
        if "ERROR" in res:
            sys.stderr.write(f"Error: {res}\n")
            sys.exit(1)

    if args.frame_skip is not None:
        res = emulator.set_frame_skip(str(args.frame_skip))
        if "ERROR" in res:
            sys.stderr.write(f"Error: {res}\n")
            sys.exit(1)

    if args.rom is not None:
        res = emulator.load_rom(args.rom)
        if "ERROR" in res:
            sys.stderr.write(f"Error: {res}\n")
            sys.exit(1)

    # Load injected input if file provided
    frame_inputs: Dict[int, Dict[str, bool]] = {}
    if args.input_inject and os.path.exists(args.input_inject):
        try:
            with open(args.input_inject, "rb") as f:
                content_bytes = f.read(1024 * 1024 + 1)
            if len(content_bytes) > 1024 * 1024:
                sys.stderr.write("Error: input-inject file too large\n")
                sys.exit(2)
            input_data = json.loads(content_bytes.decode("utf-8"))
            for item in input_data:
                frame = item.get("frame", 0)
                if not isinstance(frame, int) or frame < 0 or frame > 1000000:
                    raise ValueError("Frame index out of bounds")
                buttons = item.get("buttons", {})
                frame_inputs[frame] = buttons
        except Exception as e:
            sys.stderr.write(f"Error loading input injection: {e}\n")
            sys.exit(2)

    # Execute simulation
    if args.interactive:
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
                        for k in emulator.buttons:
                            emulator.buttons[k] = False
                    emulator.input_dirty = False

                current_frame = emulator.ticks
                if frame_inputs:
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
                            with open(arg, "rb") as f:
                                content_bytes = f.read(1024 * 1024 + 1)
                            if len(content_bytes) > 1024 * 1024:
                                sys.stdout.write("INJECT_ERROR File too large\n")
                                sys.stdout.flush()
                                continue
                            data = json.loads(content_bytes.decode("utf-8"))
                            if isinstance(data, list):
                                frame_inputs.clear()
                                for item in data:
                                    frame = item.get("frame", 0)
                                    if not isinstance(frame, int) or frame < 0 or frame > 1000000:
                                        raise ValueError("Frame index out of bounds")
                                    buttons = item.get("buttons", {})
                                    frame_inputs[frame] = buttons
                            else:
                                emulator.inject_input(data)
                            sys.stdout.write("INJECT_OK\n")
                        else:
                            arg_str = arg.strip()
                            try:
                                if arg_str.startswith("["):
                                    data = json.loads(arg_str)
                                    if not isinstance(data, list):
                                        raise ValueError("Expected a JSON array")
                                    frame_inputs.clear()
                                    for item in data:
                                        frame = item.get("frame", 0)
                                        if not isinstance(frame, int) or frame < 0 or frame > 1000000:
                                            raise ValueError("Frame index out of bounds")
                                        buttons = item.get("buttons", {})
                                        frame_inputs[frame] = buttons
                                    sys.stdout.write("INJECT_OK\n")
                                else:
                                    buttons = json.loads(arg_str)
                                    emulator.inject_input(buttons)
                                    sys.stdout.write("INJECT_OK\n")
                            except json.JSONDecodeError:
                                buttons = {
                                    "up": '"up": true' in arg_str or '"up":true' in arg_str,
                                    "down": '"down": true' in arg_str or '"down":true' in arg_str,
                                    "left": '"left": true' in arg_str or '"left":true' in arg_str,
                                    "right": '"right": true' in arg_str or '"right":true' in arg_str,
                                    "a": '"a": true' in arg_str or '"a":true' in arg_str,
                                    "b": '"b": true' in arg_str or '"b":true' in arg_str,
                                    "start": '"start": true' in arg_str or '"start":true' in arg_str,
                                    "select": '"select": true' in arg_str or '"select":true' in arg_str,
                                    "l": '"l": true' in arg_str or '"l":true' in arg_str,
                                    "r": '"r": true' in arg_str or '"r":true' in arg_str,
                                }
                                emulator.inject_input(buttons)
                                sys.stdout.write("INJECT_OK\n")
                    except Exception as e:
                        sys.stdout.write(f"INJECT_ERROR {e}\n")
                else:
                    sys.stdout.write("INJECT_ERROR Missing buttons JSON or path\n")
            elif cmd == "LOAD_ROM":
                if len(parts) > 1:
                    res = emulator.load_rom(parts[1])
                    if res == "OK":
                        sys.stdout.write("LOAD_ROM_OK\n")
                    else:
                        sys.stdout.write(f"{res}\n")
                else:
                    sys.stdout.write("LOAD_ROM_ERROR Missing ROM path\n")
            elif cmd == "SCAN_ROMS":
                if len(parts) > 1:
                    res = emulator.scan_roms(parts[1])
                    sys.stdout.write(f"{res}\n")
                else:
                    sys.stdout.write("SCAN_ROMS_ERROR Missing directory path\n")
            elif cmd == "SET_SPEED":
                if len(parts) > 1:
                    res = emulator.set_speed(parts[1])
                    if res == "OK":
                        sys.stdout.write("SET_SPEED_OK\n")
                    else:
                        sys.stdout.write(f"{res}\n")
                else:
                    sys.stdout.write("SET_SPEED_ERROR Missing speed value\n")
            elif cmd == "SET_FRAME_SKIP":
                if len(parts) > 1:
                    res = emulator.set_frame_skip(parts[1])
                    if res == "OK":
                        sys.stdout.write("SET_FRAME_SKIP_OK\n")
                    else:
                        sys.stdout.write(f"{res}\n")
                else:
                    sys.stdout.write("SET_FRAME_SKIP_ERROR Missing frame skip count\n")
            elif cmd == "SAVE_STATE":
                if len(parts) > 1:
                    res = emulator.save_state(parts[1])
                    if res == "OK":
                        sys.stdout.write("SAVE_STATE_OK\n")
                    else:
                        sys.stdout.write(f"{res}\n")
                else:
                    sys.stdout.write("SAVE_STATE_ERROR Missing slot\n")
            elif cmd == "LOAD_STATE":
                if len(parts) > 1:
                    res = emulator.load_state(parts[1])
                    if res == "OK":
                        sys.stdout.write("LOAD_STATE_OK\n")
                    else:
                        sys.stdout.write(f"{res}\n")
                else:
                    sys.stdout.write("LOAD_STATE_ERROR Missing slot\n")
            elif cmd == "DUMP_STATE":
                if len(parts) > 1:
                    path = parts[1]
                    try:
                        base_dir = "/Users/a.rudolph/Proyectos Albert/clothing_app"
                        allowed_dir = os.environ.get("ALLOWED_DUMP_DIR", base_dir)
                        if not check_path_safety(path, allowed_dir):
                            sys.stdout.write("DUMP_STATE_ERROR Path traversal detected\n")
                        else:
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
                        base_dir = "/Users/a.rudolph/Proyectos Albert/clothing_app"
                        allowed_dir = os.environ.get("ALLOWED_DUMP_DIR", base_dir)
                        if not check_path_safety(path, allowed_dir):
                            sys.stdout.write("DUMP_VIDEO_ERROR Path traversal detected\n")
                        else:
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
                        base_dir = "/Users/a.rudolph/Proyectos Albert/clothing_app"
                        allowed_dir = os.environ.get("ALLOWED_DUMP_DIR", base_dir)
                        if not check_path_safety(path, allowed_dir):
                            sys.stdout.write("DUMP_AUDIO_ERROR Path traversal detected\n")
                        else:
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
        # Non-interactive mode execution
        for _ in range(args.ticks):
            current_frame = emulator.ticks
            active_buttons = {k: False for k in emulator.buttons}
            if current_frame in frame_inputs:
                active_buttons.update(frame_inputs[current_frame])
            emulator.inject_input(active_buttons)
            emulator.tick()

        # Dumps at end of run
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
