"""Tier 5 Adversarial Test Suite for the FFI Bridge Emulator.

This suite targets edge cases, input validation gaps, integer overflows,
and denial-of-service conditions in the compiled C++ emulator binary.
"""

import json
import os
import subprocess
import sys
import tempfile
import pytest
from test_e2e import create_mock_rom


from emulator_harness import WORKSPACE, resolve_binary, spawn_interactive

EMULATOR_BIN = resolve_binary()


class TestAdversarial:
    """Tier 5: Adversarial test cases."""

    @pytest.fixture(autouse=True)
    def setup_temp_dir(self) -> None:
        """Sets up a temporary directory for each test case."""
        workspace_dir = str(WORKSPACE)
        local_temp = os.path.join(workspace_dir, "tests", "tmp")
        os.makedirs(local_temp, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=local_temp) as temp_dir:
            self.temp_dir: str = temp_dir
            yield

    def spawn_interactive(self) -> subprocess.Popen:
        """Spawns an interactive emulator process."""
        return spawn_interactive(EMULATOR_BIN, self.temp_dir)

    def test_cli_ticks_non_numeric_crash(self) -> None:
        """Test that non-numeric --ticks argument is rejected with a clean error message."""
        if EMULATOR_BIN.endswith(".py"):
            cmd = [sys.executable, EMULATOR_BIN]
        else:
            cmd = [EMULATOR_BIN]
        cmd.extend(["--headless", "--ticks", "abc"])
        
        env = os.environ.copy()
        env["ALLOWED_DUMP_DIR"] = self.temp_dir
        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5.0,
            env=env,
        )
        assert result.returncode == 1
        assert "Error: Invalid ticks value" in result.stderr

    def test_cli_ticks_overflow_crash(self) -> None:
        """Test that out-of-range/overflow --ticks argument is rejected with a clean error message."""
        if EMULATOR_BIN.endswith(".py"):
            cmd = [sys.executable, EMULATOR_BIN]
        else:
            cmd = [EMULATOR_BIN]
        cmd.extend(["--headless", "--ticks", "99999999999999999999999999999999"])
        
        env = os.environ.copy()
        env["ALLOWED_DUMP_DIR"] = self.temp_dir
        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5.0,
            env=env,
        )
        assert result.returncode == 1
        assert "Error: Invalid ticks value" in result.stderr

    def test_headless_negative_ticks_reserve_crash(self) -> None:
        """Test that negative --ticks is rejected with a clean error message."""
        if EMULATOR_BIN.endswith(".py"):
            cmd = [sys.executable, EMULATOR_BIN]
        else:
            cmd = [EMULATOR_BIN]
        cmd.extend(["--headless", "--ticks", "-5"])
        
        env = os.environ.copy()
        env["ALLOWED_DUMP_DIR"] = self.temp_dir
        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=5.0,
            env=env,
        )
        assert result.returncode == 1
        assert "Error: --ticks cannot be negative" in result.stderr

    def test_interactive_dump_path_traversal(self) -> None:
        """Test path traversal rejection with DUMP_* commands."""
        proc = self.spawn_interactive()
        try:
            parent_dir = os.path.dirname(self.temp_dir)
            target_file = os.path.join(parent_dir, f"traversal_{os.path.basename(self.temp_dir)}.json")
            
            proc.stdin.write(f"DUMP_STATE {target_file}\n")
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            
            assert response.startswith("DUMP_STATE_ERROR")
            assert not os.path.exists(target_file)
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_inject_infinite_stream_dos(self) -> None:
        """Test that injecting /dev/urandom returns INJECT_ERROR cleanly without hanging."""
        proc = self.spawn_interactive()
        try:
            proc.stdin.write("INJECT /dev/urandom\n")
            proc.stdin.flush()
            
            # Read response immediately - should return quickly because of file size limit
            response = proc.stdout.readline().strip()
            assert "INJECT_ERROR" in response
            
            # Verify the process is still running and responsive
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            play_response = proc.stdout.readline().strip()
            assert play_response == "PLAY_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_invalid_json_robustness(self) -> None:
        """Test that injecting invalid JSON structures does not crash the interactive session."""
        proc = self.spawn_interactive()
        try:
            # Malformed JSON (missing closing brace)
            proc.stdin.write('INJECT {"up": true\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"
            
            # Empty JSON braces
            proc.stdin.write('INJECT {}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # Plain brace
            proc.stdin.write('INJECT {\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # Malformed sequence structure
            proc.stdin.write('INJECT [}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # Large frame overflow inside JSON sequence
            proc.stdin.write('INJECT [{"frame": 999999999999999999999999999, "buttons": {}}]\n')
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            assert "INJECT_ERROR" in response
            
            # Check that the process is still alive and responsive after these invalid injections
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_high_speed_fallback_audio(self) -> None:
        """Test that setting speed > 4.0x in fallback audio states does not crash the emulator."""
        proc = self.spawn_interactive()
        try:
            # Set speed to 5.0 (greater than 4.0)
            proc.stdin.write("SET_SPEED 5.0\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "SET_SPEED_OK"

            # Send PAUSE to transition into a state using fallback audio
            proc.stdin.write("PAUSE\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PAUSE_OK"

            # Trigger a TICK to calculate fallback audio samples
            proc.stdin.write("TICK\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip().startswith("TICK_OK")

            # Check that the process is still alive and responsive
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_extreme_slow_speed_panic(self) -> None:
        """Reject unsafe saved speed before restoring; keep the process responsive."""
        rom_path = os.path.join(self.temp_dir, "test.gbc")
        create_mock_rom("GBC", rom_path)

        # Write CALL 0x100 instruction at entry point 0x100 to take 24 cycles
        with open(rom_path, "r+b") as f:
            f.seek(0x100)
            f.write(b"\xCD\x00\x01")

        state_json_str = """{
  "console_type": "GBC",
  "playback_state": "play",
  "ticks": 10,
  "player_x": 80,
  "player_y": 72,
  "buttons": {
    "up": false, "down": false, "left": false, "right": false,
    "a": false, "b": false, "start": false, "select": false,
    "l": false, "r": false
  },
  "speed": 0.000008,
  "frame_skip": 0,
  "cpu_cycles": 100,
  "rendered_frames": 5,
  "gbc_rom_loaded": true,
  "gbc_cpu_double_speed": true,
  "gbc_mmu_io": "0000000000000000000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
}"""

        state_path = os.path.join(self.temp_dir, "savestate_slow.sav")
        with open(state_path, "w", encoding="utf-8") as f:
            f.write(state_json_str)
        
        proc = self.spawn_interactive()
        try:
            # Load ROM
            proc.stdin.write(f"LOAD_ROM {rom_path}\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

            # Load state with slow speed and double speed
            proc.stdin.write("LOAD_STATE slow\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_STATE_ERROR Invalid speed"

            # Inject START to transition GBC from Splash to Gameplay
            proc.stdin.write('INJECT {"start": true}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # First TICK to transition from Splash to Gameplay
            proc.stdin.write("TICK\n")
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            assert response.startswith("TICK_OK")

            # Release START
            proc.stdin.write('INJECT {"start": false}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # A rejected slot must leave the running machine usable.
            proc.stdin.write("TICK\n")
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            assert response.startswith("TICK_OK")
        finally:
            proc.terminate()
            proc.wait()

    def test_savestate_flash_resize_panic(self) -> None:
        """Test that loading a savestate with a small gba_flash_data does not crash the emulator on subsequent ROM load with disk save."""
        rom_path = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", rom_path)
        
        save_path = os.path.join(self.temp_dir, "game.sav")
        with open(save_path, "wb") as f:
            f.write(b"\xFF" * (128 * 1024))

        state_data = {
            "console_type": "GBA",
            "playback_state": "pause",
            "ticks": 10,
            "player_x": 120,
            "player_y": 80,
            "buttons": {
                "up": False, "down": False, "left": False, "right": False,
                "a": False, "b": False, "start": False, "select": False,
                "l": False, "r": False
            },
            "speed": 1.0,
            "frame_skip": 0,
            "cpu_cycles": 100,
            "rendered_frames": 5,
            "gba_rom_loaded": True,
            "gba_flash_data": "FF"
        }
        
        state_path = os.path.join(self.temp_dir, "savestate_1.sav")
        with open(state_path, "w", encoding="utf-8") as f:
            json.dump(state_data, f)

        proc = self.spawn_interactive()
        try:
            # Load state 1
            proc.stdin.write("LOAD_STATE 1\n")
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            assert response == "LOAD_STATE_OK"

            # Load ROM (triggers load_flash_from_disk)
            proc.stdin.write(f"LOAD_ROM {rom_path}\n")
            proc.stdin.flush()
            response2 = proc.stdout.readline().strip()
            assert response2 == "LOAD_ROM_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_fifo_audio_rate_limits(self) -> None:
        """Test that loading a state with out-of-bounds GBA FIFO pointers does not cause panics when ticked."""
        rom_path = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", rom_path)

        state_data = {
            "console_type": "GBA",
            "playback_state": "play",
            "ticks": 10,
            "player_x": 120,
            "player_y": 80,
            "buttons": {
                "up": False, "down": False, "left": False, "right": False,
                "a": False, "b": False, "start": False, "select": False,
                "l": False, "r": False
            },
            "speed": 1.0,
            "frame_skip": 0,
            "cpu_cycles": 100,
            "rendered_frames": 5,
            "gba_rom_loaded": True,
            "gba_apu_fifo_a_read_ptr": 50, # Out of bounds pointer (max is 31)
            "gba_apu_fifo_a_count": 10,
            "gba_timer_ch0_control": 128,  # Enabled
            "gba_timer_ch0_counter": 65535, # Overflow next cycle
            "gba_timer_ch0_reload": 65535
        }

        state_path = os.path.join(self.temp_dir, "savestate_2.sav")
        with open(state_path, "w", encoding="utf-8") as f:
            json.dump(state_data, f)

        proc = self.spawn_interactive()
        try:
            # Load ROM
            proc.stdin.write(f"LOAD_ROM {rom_path}\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

            # Load State 2
            proc.stdin.write("LOAD_STATE 2\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_STATE_OK"

            # Inject START to transition GBA from Splash to Gameplay
            proc.stdin.write('INJECT {"start": true}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # Tick 1: Transitions GBA to Gameplay
            proc.stdin.write("TICK\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip().startswith("TICK_OK")

            # Release START
            proc.stdin.write('INJECT {"start": false}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # Tick 2: Runs Gameplay, triggers timer overflow & GBA FIFO pop out-of-bounds crash
            proc.stdin.write("TICK\n")
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            assert response.startswith("TICK_OK")
        finally:
            proc.terminate()
            proc.wait()

    def test_dynamic_window_resizing_stress(self) -> None:
        """Test that repeatedly switching between GBC and GBA ROMs (dynamic window resizing) does not crash the interactive emulator."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBC", gbc_rom)
        create_mock_rom("GBA", gba_rom)

        proc = self.spawn_interactive()
        try:
            for _ in range(5):
                proc.stdin.write(f"LOAD_ROM {gbc_rom}\n")
                proc.stdin.flush()
                assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

                proc.stdin.write("TICK\n")
                proc.stdin.flush()
                assert proc.stdout.readline().strip().startswith("TICK_OK")

                proc.stdin.write(f"LOAD_ROM {gba_rom}\n")
                proc.stdin.flush()
                assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

                proc.stdin.write("TICK\n")
                proc.stdin.flush()
                assert proc.stdout.readline().strip().startswith("TICK_OK")
        finally:
            proc.terminate()
            proc.wait()

    def test_scan_roms_path_traversal(self) -> None:
        """Test path traversal rejection with SCAN_ROMS command."""
        proc = self.spawn_interactive()
        try:
            proc.stdin.write("SCAN_ROMS ../../..\n")
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            assert "SCAN_ROMS_ERROR" in response or "Path traversal detected" in response
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_mbc3_rtc_latching(self) -> None:
        """Test GBC MBC3 RTC latching state preservation via load/save state."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", gbc_rom)

        proc = self.spawn_interactive()
        try:
            # Load ROM first
            proc.stdin.write(f"LOAD_ROM {gbc_rom}\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

            state_data = {
                "console_type": "GBC",
                "playback_state": "pause",
                "ticks": 5,
                "player_x": 80,
                "player_y": 72,
                "speed": 1.0,
                "frame_skip": 0,
                "cpu_cycles": 1000,
                "rendered_frames": 5,
                "buttons": {
                    "up": False, "down": False, "left": False, "right": False,
                    "a": False, "b": False, "start": False, "select": False,
                    "l": False, "r": False
                },
                "gbc_rom_loaded": True,
                "gbc_cpu_pc": 256,
                "gbc_cpu_sp": 65534,
                "gbc_cpu_a": 0, "gbc_cpu_f": 0, "gbc_cpu_b": 0, "gbc_cpu_c": 0,
                "gbc_cpu_d": 0, "gbc_cpu_e": 0, "gbc_cpu_h": 0, "gbc_cpu_l": 0,
                "gbc_cpu_ime": False, "gbc_cpu_halted": False,
                "gbc_cpu_double_speed": False, "gbc_cpu_ei_delay": False,
                "gbc_cpu_stop_mode": False, "gbc_cpu_stop_cycles_left": 0,
                "gbc_cpu_div_counter": 0, "gbc_mmu_ie": 0, "gbc_mmu_bcps": 0, "gbc_mmu_ocps": 0,
                "gbc_mmu_mbc_rom_bank": 1, "gbc_mmu_mbc_ram_bank_or_rtc_reg": 0,
                "gbc_mmu_mbc_ram_rtc_enabled": True, "gbc_mmu_mbc_latch_state": 1,
                "gbc_mmu_mbc_rtc_seconds": 45, "gbc_mmu_mbc_rtc_minutes": 30,
                "gbc_mmu_mbc_rtc_hours": 12, "gbc_mmu_mbc_rtc_days": 100,
                "gbc_mmu_mbc_rtc_halt": False, "gbc_mmu_mbc_rtc_day_overflow": False,
                "gbc_mmu_mbc_rtc_cycle_accumulator": 0,
                "gbc_mmu_mbc_rtc_latched_seconds": 15, "gbc_mmu_mbc_rtc_latched_minutes": 25,
                "gbc_mmu_mbc_rtc_latched_hours": 10, "gbc_mmu_mbc_rtc_latched_days_low": 99,
                "gbc_mmu_mbc_rtc_latched_days_high": 0,
                "gbc_mmu_vram": "00" * 16384,
                "gbc_mmu_wram": "00" * 32768,
                "gbc_mmu_oam": "00" * 160,
                "gbc_mmu_io": "00" * 128,
                "gbc_mmu_hram": "00" * 127,
                "gbc_mmu_bg_palette_ram": "00" * 64,
                "gbc_mmu_obj_palette_ram": "00" * 64,
                "gbc_mmu_mbc_ram": "00" * 32768,
                "gbc_ppu_cycle_accumulator": 0,
                "gbc_apu_frame_seq_timer": 0, "gbc_apu_frame_seq_step": 0,
                "gbc_apu_ch1_enabled": False, "gbc_apu_ch1_duty": 0,
                "gbc_apu_ch1_duty_pointer": 0, "gbc_apu_ch1_length_enabled": False,
                "gbc_apu_ch1_length_counter": 0, "gbc_apu_ch1_period": 0,
                "gbc_apu_ch1_period_timer": 0, "gbc_apu_ch1_volume": 0,
                "gbc_apu_ch1_env_enabled": False, "gbc_apu_ch1_env_period": 0,
                "gbc_apu_ch1_env_timer": 0, "gbc_apu_ch1_env_direction": 0,
                "gbc_apu_ch1_env_initial_volume": 0, "gbc_apu_ch1_sweep_enabled": False,
                "gbc_apu_ch1_sweep_period": 0, "gbc_apu_ch1_sweep_timer": 0,
                "gbc_apu_ch1_sweep_shift": 0, "gbc_apu_ch1_sweep_direction": 0,
                "gbc_apu_ch1_shadow_frequency": 0, "gbc_apu_ch2_enabled": False,
                "gbc_apu_ch2_duty": 0, "gbc_apu_ch2_duty_pointer": 0,
                "gbc_apu_ch2_length_enabled": False, "gbc_apu_ch2_length_counter": 0,
                "gbc_apu_ch2_period": 0, "gbc_apu_ch2_period_timer": 0,
                "gbc_apu_ch2_volume": 0, "gbc_apu_ch2_env_enabled": False,
                "gbc_apu_ch2_env_period": 0, "gbc_apu_ch2_env_timer": 0,
                "gbc_apu_ch2_env_direction": 0, "gbc_apu_ch2_env_initial_volume": 0,
                "gbc_apu_ch3_enabled": False, "gbc_apu_ch3_dac_enabled": False,
                "gbc_apu_ch3_length_enabled": False, "gbc_apu_ch3_length_counter": 0,
                "gbc_apu_ch3_period": 0, "gbc_apu_ch3_period_timer": 0,
                "gbc_apu_ch3_volume_shift": 0, "gbc_apu_ch3_wave_ram": "00" * 16,
                "gbc_apu_ch3_sample_pointer": 0, "gbc_apu_ch4_enabled": False,
                "gbc_apu_ch4_length_enabled": False, "gbc_apu_ch4_length_counter": 0,
                "gbc_apu_ch4_volume": 0, "gbc_apu_ch4_env_enabled": False,
                "gbc_apu_ch4_env_period": 0, "gbc_apu_ch4_env_timer": 0,
                "gbc_apu_ch4_env_direction": 0, "gbc_apu_ch4_env_initial_volume": 0,
                "gbc_apu_ch4_lfsr": 0, "gbc_apu_ch4_divisor": 0,
                "gbc_apu_ch4_shift_clock": 0, "gbc_apu_ch4_width_7bit": False,
                "gbc_apu_ch4_period_timer": 0
            }

            state_path = os.path.join(self.temp_dir, "savestate_test_rtc.sav")
            with open(state_path, "w", encoding="utf-8") as f:
                json.dump(state_data, f)

            # Load the state
            proc.stdin.write("LOAD_STATE test_rtc\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_STATE_OK"

            # Save the state back
            proc.stdin.write("SAVE_STATE test_rtc_out\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "SAVE_STATE_OK"

            # Read the output state file and verify RTC latching values
            out_state_path = os.path.join(self.temp_dir, "savestate_test_rtc_out.sav")
            with open(out_state_path, "r", encoding="utf-8") as f:
                dumped_data = json.load(f)

            # Check RTC fields
            assert dumped_data.get("gbc_mmu_mbc_rtc_seconds") == 45
            assert dumped_data.get("gbc_mmu_mbc_rtc_latched_seconds") == 15
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_dma_overlaps(self) -> None:
        """Test GBA DMA transfer overlaps and priority handling under active state."""
        rom_path = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", rom_path)

        state_data = {
            "console_type": "GBA",
            "playback_state": "pause",
            "ticks": 10,
            "player_x": 120,
            "player_y": 80,
            "buttons": {
                "up": False, "down": False, "left": False, "right": False,
                "a": False, "b": False, "start": False, "select": False,
                "l": False, "r": False
            },
            "speed": 1.0,
            "frame_skip": 0,
            "cpu_cycles": 100,
            "rendered_frames": 5,
            "gba_rom_loaded": True,
            "gba_cpu_r0": 0, "gba_cpu_r1": 0, "gba_cpu_r2": 0, "gba_cpu_r3": 0,
            "gba_cpu_r4": 0, "gba_cpu_r5": 0, "gba_cpu_r6": 0, "gba_cpu_r7": 0,
            "gba_cpu_r8": 0, "gba_cpu_r9": 0, "gba_cpu_r10": 0, "gba_cpu_r11": 0,
            "gba_cpu_r12": 0, "gba_cpu_r13": 0, "gba_cpu_r14": 0, "gba_cpu_r15": 0x02000000,
            "gba_cpu_cpsr": 0x1F, "gba_cpu_spsr": 0x1F, "gba_cpu_halted": False,
            "gba_mmu_waitcnt": 0, "gba_mmu_ie": 0, "gba_mmu_if": 0, "gba_mmu_ime": 0,
            "gba_flash_bank": 0, "gba_flash_state": 0,
            "gba_cpu_r8_usr": [0, 0, 0, 0, 0, 0, 0],
            "gba_cpu_r8_fiq": [0, 0, 0, 0, 0, 0, 0],
            "gba_cpu_r13_usr": 0, "gba_cpu_r14_usr": 0,
            "gba_cpu_r13_svc": 0, "gba_cpu_r14_svc": 0, "gba_cpu_spsr_svc": 0,
            "gba_cpu_r13_irq": 0, "gba_cpu_r14_irq": 0, "gba_cpu_spsr_irq": 0,
            "gba_cpu_r13_abt": 0, "gba_cpu_r14_abt": 0, "gba_cpu_spsr_abt": 0,
            "gba_cpu_r13_und": 0, "gba_cpu_r14_und": 0, "gba_cpu_spsr_und": 0,
            "gba_cpu_r13_fiq": 0, "gba_cpu_r14_fiq": 0, "gba_cpu_spsr_fiq": 0,
            "gba_cpu_pipeline": [0, 0],
            "gba_apu_fifo_a_buffer": "00" * 32, "gba_apu_fifo_a_write_ptr": 0, "gba_apu_fifo_a_read_ptr": 0, "gba_apu_fifo_a_count": 0,
            "gba_apu_fifo_b_buffer": "00" * 32, "gba_apu_fifo_b_write_ptr": 0, "gba_apu_fifo_b_read_ptr": 0, "gba_apu_fifo_b_count": 0,
            "gba_apu_dma_request_a": False, "gba_apu_dma_request_b": False,
            "gba_apu_current_sample_a": 0, "gba_apu_current_sample_b": 0,
            "gba_dma_ch0_sad": 0x02000000, "gba_dma_ch0_dad": 0x02001000,
            "gba_dma_ch0_count": 16, "gba_dma_ch0_control": 0x8400,
            "gba_dma_ch0_cur_src": 0x02000000, "gba_dma_ch0_cur_dest": 0x02001000,
            "gba_dma_ch0_cur_count": 16, "gba_dma_ch0_active": True,
            "gba_dma_ch1_sad": 0x02001000, "gba_dma_ch1_dad": 0x02002000,
            "gba_dma_ch1_count": 16, "gba_dma_ch1_control": 0x8400,
            "gba_dma_ch1_cur_src": 0x02001000, "gba_dma_ch1_cur_dest": 0x02002000,
            "gba_dma_ch1_cur_count": 16, "gba_dma_ch1_active": True,
            "gba_dma_ch2_sad": 0, "gba_dma_ch2_dad": 0, "gba_dma_ch2_count": 0, "gba_dma_ch2_control": 0,
            "gba_dma_ch2_cur_src": 0, "gba_dma_ch2_cur_dest": 0, "gba_dma_ch2_cur_count": 0, "gba_dma_ch2_active": False,
            "gba_dma_ch3_sad": 0, "gba_dma_ch3_dad": 0, "gba_dma_ch3_count": 0, "gba_dma_ch3_control": 0,
            "gba_dma_ch3_cur_src": 0, "gba_dma_ch3_cur_dest": 0, "gba_dma_ch3_cur_count": 0, "gba_dma_ch3_active": False,
            "gba_timer_ch0_counter": 0, "gba_timer_ch0_reload": 0, "gba_timer_ch0_control": 0, "gba_timer_ch0_cycle_accumulator": 0, "gba_timer_ch0_overflowed": False,
            "gba_timer_ch1_counter": 0, "gba_timer_ch1_reload": 0, "gba_timer_ch1_control": 0, "gba_timer_ch1_cycle_accumulator": 0, "gba_timer_ch1_overflowed": False,
            "gba_timer_ch2_counter": 0, "gba_timer_ch2_reload": 0, "gba_timer_ch2_control": 0, "gba_timer_ch2_cycle_accumulator": 0, "gba_timer_ch2_overflowed": False,
            "gba_timer_ch3_counter": 0, "gba_timer_ch3_reload": 0, "gba_timer_ch3_control": 0, "gba_timer_ch3_cycle_accumulator": 0, "gba_timer_ch3_overflowed": False
        }

        state_path = os.path.join(self.temp_dir, "savestate_test_dma.sav")
        with open(state_path, "w", encoding="utf-8") as f:
            json.dump(state_data, f)

        proc = self.spawn_interactive()
        try:
            # Load ROM
            proc.stdin.write(f"LOAD_ROM {rom_path}\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

            # Load state
            proc.stdin.write("LOAD_STATE test_dma\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_STATE_OK"

            # Play and Tick the emulator
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"

            proc.stdin.write("TICK\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip().startswith("TICK_OK")

            # Check that emulator didn't crash and is responsive
            proc.stdin.write("PAUSE\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PAUSE_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_invalid_gbc_instruction(self) -> None:
        """Test that invalid GBC instructions are handled cleanly (acting as NOPs) without hanging or crashing."""
        rom_path = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", rom_path)

        state_data = {
            "console_type": "GBC",
            "playback_state": "pause",
            "ticks": 10,
            "player_x": 80,
            "player_y": 72,
            "speed": 1.0,
            "frame_skip": 0,
            "cpu_cycles": 100,
            "rendered_frames": 5,
            "buttons": {
                "up": False, "down": False, "left": False, "right": False,
                "a": False, "b": False, "start": False, "select": False,
                "l": False, "r": False
            },
            "gbc_rom_loaded": True,
            "gbc_cpu_pc": 0xC000,
            "gbc_cpu_sp": 0xFFFE,
            "gbc_cpu_a": 0, "gbc_cpu_f": 0, "gbc_cpu_b": 0, "gbc_cpu_c": 0,
            "gbc_cpu_d": 0, "gbc_cpu_e": 0, "gbc_cpu_h": 0, "gbc_cpu_l": 0,
            "gbc_cpu_ime": False, "gbc_cpu_halted": False,
            "gbc_cpu_double_speed": False, "gbc_cpu_ei_delay": False,
            "gbc_cpu_stop_mode": False, "gbc_cpu_stop_cycles_left": 0,
            "gbc_cpu_div_counter": 0, "gbc_mmu_ie": 0, "gbc_mmu_bcps": 0, "gbc_mmu_ocps": 0,
            "gbc_mmu_mbc_rom_bank": 1, "gbc_mmu_mbc_ram_bank_or_rtc_reg": 0,
            "gbc_mmu_mbc_ram_rtc_enabled": False, "gbc_mmu_mbc_latch_state": 0xFF,
            "gbc_mmu_mbc_rtc_seconds": 0, "gbc_mmu_mbc_rtc_minutes": 0,
            "gbc_mmu_mbc_rtc_hours": 0, "gbc_mmu_mbc_rtc_days": 0,
            "gbc_mmu_mbc_rtc_halt": False, "gbc_mmu_mbc_rtc_day_overflow": False,
            "gbc_mmu_mbc_rtc_cycle_accumulator": 0,
            "gbc_mmu_mbc_rtc_latched_seconds": 0, "gbc_mmu_mbc_rtc_latched_minutes": 0,
            "gbc_mmu_mbc_rtc_latched_hours": 0, "gbc_mmu_mbc_rtc_latched_days_low": 0,
            "gbc_mmu_mbc_rtc_latched_days_high": 0,
            "gbc_mmu_vram": "00" * 16384,
            "gbc_mmu_wram": "DDEDFCFD" * 10 + "76" + "00" * (32768 - 41),
            "gbc_mmu_oam": "00" * 160,
            "gbc_mmu_io": "00" * 128,
            "gbc_mmu_hram": "00" * 127,
            "gbc_mmu_bg_palette_ram": "00" * 64,
            "gbc_mmu_obj_palette_ram": "00" * 64,
            "gbc_mmu_mbc_ram": "00" * 32768,
            "gbc_ppu_cycle_accumulator": 0,
            "gbc_apu_frame_seq_timer": 0, "gbc_apu_frame_seq_step": 0,
            "gbc_apu_ch1_enabled": False, "gbc_apu_ch1_duty": 0,
            "gbc_apu_ch1_duty_pointer": 0, "gbc_apu_ch1_length_enabled": False,
            "gbc_apu_ch1_length_counter": 0, "gbc_apu_ch1_period": 0,
            "gbc_apu_ch1_period_timer": 0, "gbc_apu_ch1_volume": 0,
            "gbc_apu_ch1_env_enabled": False, "gbc_apu_ch1_env_period": 0,
            "gbc_apu_ch1_env_timer": 0, "gbc_apu_ch1_env_direction": 0,
            "gbc_apu_ch1_env_initial_volume": 0, "gbc_apu_ch1_sweep_enabled": False,
            "gbc_apu_ch1_sweep_period": 0, "gbc_apu_ch1_sweep_timer": 0,
            "gbc_apu_ch1_sweep_shift": 0, "gbc_apu_ch1_sweep_direction": 0,
            "gbc_apu_ch1_shadow_frequency": 0, "gbc_apu_ch2_enabled": False,
            "gbc_apu_ch2_duty": 0, "gbc_apu_ch2_duty_pointer": 0,
            "gbc_apu_ch2_length_enabled": False, "gbc_apu_ch2_length_counter": 0,
            "gbc_apu_ch2_period": 0, "gbc_apu_ch2_period_timer": 0,
            "gbc_apu_ch2_volume": 0, "gbc_apu_ch2_env_enabled": False,
            "gbc_apu_ch2_env_period": 0, "gbc_apu_ch2_env_timer": 0,
            "gbc_apu_ch2_env_direction": 0, "gbc_apu_ch2_env_initial_volume": 0,
            "gbc_apu_ch3_enabled": False, "gbc_apu_ch3_dac_enabled": False,
            "gbc_apu_ch3_length_enabled": False, "gbc_apu_ch3_length_counter": 0,
            "gbc_apu_ch3_period": 0, "gbc_apu_ch3_period_timer": 0,
            "gbc_apu_ch3_volume_shift": 0, "gbc_apu_ch3_wave_ram": "00" * 16,
            "gbc_apu_ch3_sample_pointer": 0, "gbc_apu_ch4_enabled": False,
            "gbc_apu_ch4_length_enabled": False, "gbc_apu_ch4_length_counter": 0,
            "gbc_apu_ch4_volume": 0, "gbc_apu_ch4_env_enabled": False,
            "gbc_apu_ch4_env_period": 0, "gbc_apu_ch4_env_timer": 0,
            "gbc_apu_ch4_env_direction": 0, "gbc_apu_ch4_env_initial_volume": 0,
            "gbc_apu_ch4_lfsr": 0, "gbc_apu_ch4_divisor": 0,
            "gbc_apu_ch4_shift_clock": 0, "gbc_apu_ch4_width_7bit": False,
            "gbc_apu_ch4_period_timer": 0
        }

        state_path = os.path.join(self.temp_dir, "savestate_test_invalid_gbc.sav")
        with open(state_path, "w", encoding="utf-8") as f:
            json.dump(state_data, f)

        proc = self.spawn_interactive()
        try:
            # Load ROM
            proc.stdin.write(f"LOAD_ROM {rom_path}\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

            # Load state
            proc.stdin.write("LOAD_STATE test_invalid_gbc\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_STATE_OK"

            # Play and Tick the emulator
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"

            proc.stdin.write("TICK\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip().startswith("TICK_OK")

            # Verify it is responsive
            proc.stdin.write("PAUSE\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PAUSE_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_invalid_gba_instruction(self) -> None:
        """Test that invalid GBA instructions are handled cleanly without crashing or hanging."""
        rom_path = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBA", rom_path)

        state_data = {
            "console_type": "GBA",
            "playback_state": "pause",
            "ticks": 10,
            "player_x": 120,
            "player_y": 80,
            "buttons": {
                "up": False, "down": False, "left": False, "right": False,
                "a": False, "b": False, "start": False, "select": False,
                "l": False, "r": False
            },
            "speed": 1.0,
            "frame_skip": 0,
            "cpu_cycles": 100,
            "rendered_frames": 5,
            "gba_rom_loaded": True,
            "gba_cpu_r0": 0, "gba_cpu_r1": 0, "gba_cpu_r2": 0, "gba_cpu_r3": 0,
            "gba_cpu_r4": 0, "gba_cpu_r5": 0, "gba_cpu_r6": 0, "gba_cpu_r7": 0,
            "gba_cpu_r8": 0, "gba_cpu_r9": 0, "gba_cpu_r10": 0, "gba_cpu_r11": 0,
            "gba_cpu_r12": 0, "gba_cpu_r13": 0, "gba_cpu_r14": 0, "gba_cpu_r15": 0x02000000,
            "gba_cpu_cpsr": 0x1F, "gba_cpu_spsr": 0x1F, "gba_cpu_halted": False,
            "gba_mmu_waitcnt": 0, "gba_mmu_ie": 0, "gba_mmu_if": 0, "gba_mmu_ime": 0,
            "gba_flash_bank": 0, "gba_flash_state": 0,
            "gba_mmu_ewram": "FFFFFFFF" * 100 + "00" * (262144 - 400),
            "gba_cpu_r8_usr": [0, 0, 0, 0, 0, 0, 0],
            "gba_cpu_r8_fiq": [0, 0, 0, 0, 0, 0, 0],
            "gba_cpu_r13_usr": 0, "gba_cpu_r14_usr": 0,
            "gba_cpu_r13_svc": 0, "gba_cpu_r14_svc": 0, "gba_cpu_spsr_svc": 0,
            "gba_cpu_r13_irq": 0, "gba_cpu_r14_irq": 0, "gba_cpu_spsr_irq": 0,
            "gba_cpu_r13_abt": 0, "gba_cpu_r14_abt": 0, "gba_cpu_spsr_abt": 0,
            "gba_cpu_r13_und": 0, "gba_cpu_r14_und": 0, "gba_cpu_spsr_und": 0,
            "gba_cpu_r13_fiq": 0, "gba_cpu_r14_fiq": 0, "gba_cpu_spsr_fiq": 0,
            "gba_cpu_pipeline": [0xFFFFFFFF, 0xFFFFFFFF],
            "gba_apu_fifo_a_buffer": "00" * 32, "gba_apu_fifo_a_write_ptr": 0, "gba_apu_fifo_a_read_ptr": 0, "gba_apu_fifo_a_count": 0,
            "gba_apu_fifo_b_buffer": "00" * 32, "gba_apu_fifo_b_write_ptr": 0, "gba_apu_fifo_b_read_ptr": 0, "gba_apu_fifo_b_count": 0,
            "gba_apu_dma_request_a": False, "gba_apu_dma_request_b": False,
            "gba_apu_current_sample_a": 0, "gba_apu_current_sample_b": 0,
            "gba_dma_ch0_sad": 0, "gba_dma_ch0_dad": 0, "gba_dma_ch0_count": 0, "gba_dma_ch0_control": 0, "gba_dma_ch0_cur_src": 0, "gba_dma_ch0_cur_dest": 0, "gba_dma_ch0_cur_count": 0, "gba_dma_ch0_active": False,
            "gba_dma_ch1_sad": 0, "gba_dma_ch1_dad": 0, "gba_dma_ch1_count": 0, "gba_dma_ch1_control": 0, "gba_dma_ch1_cur_src": 0, "gba_dma_ch1_cur_dest": 0, "gba_dma_ch1_cur_count": 0, "gba_dma_ch1_active": False,
            "gba_dma_ch2_sad": 0, "gba_dma_ch2_dad": 0, "gba_dma_ch2_count": 0, "gba_dma_ch2_control": 0, "gba_dma_ch2_cur_src": 0, "gba_dma_ch2_cur_dest": 0, "gba_dma_ch2_cur_count": 0, "gba_dma_ch2_active": False,
            "gba_dma_ch3_sad": 0, "gba_dma_ch3_dad": 0, "gba_dma_ch3_count": 0, "gba_dma_ch3_control": 0, "gba_dma_ch3_cur_src": 0, "gba_dma_ch3_cur_dest": 0, "gba_dma_ch3_cur_count": 0, "gba_dma_ch3_active": False,
            "gba_timer_ch0_counter": 0, "gba_timer_ch0_reload": 0, "gba_timer_ch0_control": 0, "gba_timer_ch0_cycle_accumulator": 0, "gba_timer_ch0_overflowed": False,
            "gba_timer_ch1_counter": 0, "gba_timer_ch1_reload": 0, "gba_timer_ch1_control": 0, "gba_timer_ch1_cycle_accumulator": 0, "gba_timer_ch1_overflowed": False,
            "gba_timer_ch2_counter": 0, "gba_timer_ch2_reload": 0, "gba_timer_ch2_control": 0, "gba_timer_ch2_cycle_accumulator": 0, "gba_timer_ch2_overflowed": False,
            "gba_timer_ch3_counter": 0, "gba_timer_ch3_reload": 0, "gba_timer_ch3_control": 0, "gba_timer_ch3_cycle_accumulator": 0, "gba_timer_ch3_overflowed": False
        }

        state_path = os.path.join(self.temp_dir, "savestate_test_invalid_gba.sav")
        with open(state_path, "w", encoding="utf-8") as f:
            json.dump(state_data, f)

        proc = self.spawn_interactive()
        try:
            # Load ROM
            proc.stdin.write(f"LOAD_ROM {rom_path}\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

            # Load state
            proc.stdin.write("LOAD_STATE test_invalid_gba\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_STATE_OK"

            # Play and Tick the emulator
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"

            proc.stdin.write("TICK\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip().startswith("TICK_OK")

            # Verify responsive
            proc.stdin.write("PAUSE\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PAUSE_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_interactive_input_boundary_controls(self) -> None:
        """Test input boundary conditions, conflicting button combinations, malformed JSON, and massive injections."""
        proc = self.spawn_interactive()
        try:
            # 1. Conflicting buttons: LEFT and RIGHT both true
            proc.stdin.write('INJECT {"left": true, "right": true}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # 2. Conflicting buttons: UP and DOWN both true
            proc.stdin.write('INJECT {"up": true, "down": true}\n')
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "INJECT_OK"

            # 3. Invalid JSON types / Out-of-bounds numbers inside buttons values
            proc.stdin.write('INJECT {"up": 123456789, "down": "NOT_A_BOOL"}\n')
            proc.stdin.flush()
            # The parser tries to read keys. It will either treat it as false/true or gracefully parse it.
            # Either way it must NOT crash the interactive shell.
            assert "INJECT" in proc.stdout.readline().strip()

            # 4. Extremely massive input sequence (large JSON array with many frames)
            massive_sequence = [{"frame": i, "buttons": {"a": True}} for i in range(1000)]
            massive_json = json.dumps(massive_sequence)

            seq_path = os.path.join(self.temp_dir, "massive_seq.json")
            with open(seq_path, "w", encoding="utf-8") as f:
                f.write(massive_json)

            proc.stdin.write(f"INJECT {seq_path}\n")
            proc.stdin.flush()
            response = proc.stdout.readline().strip()
            # Should be OK or ERROR cleanly, but absolutely no crash
            assert "INJECT_OK" in response or "INJECT_ERROR" in response

            # 5. Out of bounds frame number injection
            proc.stdin.write('INJECT [{"frame": -99999, "buttons": {}}]\n')
            proc.stdin.flush()
            assert "INJECT" in proc.stdout.readline().strip()

            # 6. Verify responsiveness
            proc.stdin.write("PLAY\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "PLAY_OK"
        finally:
            proc.terminate()
            proc.wait()

    def test_resolution_switching_limits(self) -> None:
        """Test rapid switching between resolutions GBC/GBA under tick pressure to check alignment and realloc limits."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        gba_rom = os.path.join(self.temp_dir, "game.gba")
        create_mock_rom("GBC", gbc_rom)
        create_mock_rom("GBA", gba_rom)

        proc = self.spawn_interactive()
        try:
            # We switch GBC/GBA 20 times to stress reallocations
            for _ in range(20):
                proc.stdin.write(f"LOAD_ROM {gbc_rom}\n")
                proc.stdin.flush()
                assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

                proc.stdin.write("PLAY\n")
                proc.stdin.flush()
                assert proc.stdout.readline().strip() == "PLAY_OK"

                proc.stdin.write("TICK\n")
                proc.stdin.flush()
                assert proc.stdout.readline().strip().startswith("TICK_OK")

                proc.stdin.write(f"LOAD_ROM {gba_rom}\n")
                proc.stdin.flush()
                assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

                proc.stdin.write("TICK\n")
                proc.stdin.flush()
                assert proc.stdout.readline().strip().startswith("TICK_OK")
        finally:
            proc.terminate()
            proc.wait()

    def test_savestate_extra_fields_preservation(self) -> None:
        """Test non-destructive support for custom/extra fields in savestates."""
        gbc_rom = os.path.join(self.temp_dir, "game.gbc")
        create_mock_rom("GBC", gbc_rom)

        state_data = {
            "console_type": "GBC",
            "playback_state": "pause",
            "ticks": 5,
            "player_x": 80,
            "player_y": 72,
            "speed": 1.0,
            "frame_skip": 0,
            "cpu_cycles": 1000,
            "rendered_frames": 5,
            "buttons": {
                "up": False, "down": False, "left": False, "right": False,
                "a": False, "b": False, "start": False, "select": False,
                "l": False, "r": False
            },
            "gbc_rom_loaded": True,
            "gbc_cpu_pc": 256,
            "gbc_cpu_sp": 65534,
            "gbc_cpu_a": 0, "gbc_cpu_f": 0, "gbc_cpu_b": 0, "gbc_cpu_c": 0,
            "gbc_cpu_d": 0, "gbc_cpu_e": 0, "gbc_cpu_h": 0, "gbc_cpu_l": 0,
            "gbc_cpu_ime": False, "gbc_cpu_halted": False,
            "gbc_cpu_double_speed": False, "gbc_cpu_ei_delay": False,
            "gbc_cpu_stop_mode": False, "gbc_cpu_stop_cycles_left": 0,
            "gbc_cpu_div_counter": 0, "gbc_mmu_ie": 0, "gbc_mmu_bcps": 0, "gbc_mmu_ocps": 0,
            "gbc_mmu_mbc_rom_bank": 1, "gbc_mmu_mbc_ram_bank_or_rtc_reg": 0,
            "gbc_mmu_mbc_ram_rtc_enabled": True, "gbc_mmu_mbc_latch_state": 1,
            "gbc_mmu_mbc_rtc_seconds": 45, "gbc_mmu_mbc_rtc_minutes": 30,
            "gbc_mmu_mbc_rtc_hours": 12, "gbc_mmu_mbc_rtc_days": 100,
            "gbc_mmu_mbc_rtc_halt": False, "gbc_mmu_mbc_rtc_day_overflow": False,
            "gbc_mmu_mbc_rtc_cycle_accumulator": 0,
            "gbc_mmu_mbc_rtc_latched_seconds": 15, "gbc_mmu_mbc_rtc_latched_minutes": 25,
            "gbc_mmu_mbc_rtc_latched_hours": 10, "gbc_mmu_mbc_rtc_latched_days_low": 99,
            "gbc_mmu_mbc_rtc_latched_days_high": 0,
            "gbc_mmu_vram": "00" * 16384,
            "gbc_mmu_wram": "00" * 32768,
            "gbc_mmu_oam": "00" * 160,
            "gbc_mmu_io": "00" * 128,
            "gbc_mmu_hram": "00" * 127,
            "gbc_mmu_bg_palette_ram": "00" * 64,
            "gbc_mmu_obj_palette_ram": "00" * 64,
            "gbc_mmu_mbc_ram": "00" * 32768,
            "gbc_ppu_cycle_accumulator": 0,
            "gbc_apu_frame_seq_timer": 0, "gbc_apu_frame_seq_step": 0,
            "gbc_apu_ch1_enabled": False, "gbc_apu_ch1_duty": 0,
            "gbc_apu_ch1_duty_pointer": 0, "gbc_apu_ch1_length_enabled": False,
            "gbc_apu_ch1_length_counter": 0, "gbc_apu_ch1_period": 0,
            "gbc_apu_ch1_period_timer": 0, "gbc_apu_ch1_volume": 0,
            "gbc_apu_ch1_env_enabled": False, "gbc_apu_ch1_env_period": 0,
            "gbc_apu_ch1_env_timer": 0, "gbc_apu_ch1_env_direction": 0,
            "gbc_apu_ch1_env_initial_volume": 0, "gbc_apu_ch1_sweep_enabled": False,
            "gbc_apu_ch1_sweep_period": 0, "gbc_apu_ch1_sweep_timer": 0,
            "gbc_apu_ch1_sweep_shift": 0, "gbc_apu_ch1_sweep_direction": 0,
            "gbc_apu_ch1_shadow_frequency": 0, "gbc_apu_ch2_enabled": False,
            "gbc_apu_ch2_duty": 0, "gbc_apu_ch2_duty_pointer": 0,
            "gbc_apu_ch2_length_enabled": False, "gbc_apu_ch2_length_counter": 0,
            "gbc_apu_ch2_period": 0, "gbc_apu_ch2_period_timer": 0,
            "gbc_apu_ch2_volume": 0, "gbc_apu_ch2_env_enabled": False,
            "gbc_apu_ch2_env_period": 0, "gbc_apu_ch2_env_timer": 0,
            "gbc_apu_ch2_env_direction": 0, "gbc_apu_ch2_env_initial_volume": 0,
            "gbc_apu_ch3_enabled": False, "gbc_apu_ch3_dac_enabled": False,
            "gbc_apu_ch3_length_enabled": False, "gbc_apu_ch3_length_counter": 0,
            "gbc_apu_ch3_period": 0, "gbc_apu_ch3_period_timer": 0,
            "gbc_apu_ch3_volume_shift": 0, "gbc_apu_ch3_wave_ram": "00" * 16,
            "gbc_apu_ch3_sample_pointer": 0, "gbc_apu_ch4_enabled": False,
            "gbc_apu_ch4_length_enabled": False, "gbc_apu_ch4_length_counter": 0,
            "gbc_apu_ch4_volume": 0, "gbc_apu_ch4_env_enabled": False,
            "gbc_apu_ch4_env_period": 0, "gbc_apu_ch4_env_timer": 0,
            "gbc_apu_ch4_env_direction": 0, "gbc_apu_ch4_env_initial_volume": 0,
            "gbc_apu_ch4_lfsr": 0, "gbc_apu_ch4_divisor": 0,
            "gbc_apu_ch4_shift_clock": 0, "gbc_apu_ch4_width_7bit": False,
            "gbc_apu_ch4_period_timer": 0,
            # Custom / extra fields
            "my_custom_string": "hello",
            "my_custom_number": 123.45,
            "my_custom_boolean": True,
            "my_custom_array": [1, 2, 3],
            "my_custom_object": {"a": 1}
        }

        state_path = os.path.join(self.temp_dir, "savestate_test_custom.sav")
        with open(state_path, "w", encoding="utf-8") as f:
            json.dump(state_data, f)

        proc = self.spawn_interactive()
        try:
            # Load ROM first
            proc.stdin.write(f"LOAD_ROM {gbc_rom}\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_ROM_OK"

            # Load the state containing custom fields
            proc.stdin.write("LOAD_STATE test_custom\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "LOAD_STATE_OK"

            # Save the state back
            proc.stdin.write("SAVE_STATE test_custom_out\n")
            proc.stdin.flush()
            assert proc.stdout.readline().strip() == "SAVE_STATE_OK"

            # Read the output state file and verify custom fields
            out_state_path = os.path.join(self.temp_dir, "savestate_test_custom_out.sav")
            with open(out_state_path, "r", encoding="utf-8") as f:
                dumped_data = json.load(f)

            # Check preservation of custom fields
            assert dumped_data.get("my_custom_string") == "hello"
            assert dumped_data.get("my_custom_number") == 123.45
            assert dumped_data.get("my_custom_boolean") is True
            assert dumped_data.get("my_custom_array") == [1, 2, 3]
            assert dumped_data.get("my_custom_object") == {"a": 1}
        finally:
            proc.terminate()
            proc.wait()



