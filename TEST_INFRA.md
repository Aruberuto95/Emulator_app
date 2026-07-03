# E2E Test Infra: Universal GBC and GBA Emulator

## Test Philosophy
- Opaque-box, requirement-driven. Tests verify the FFI and emulator behavior via CLI options and interactive stdin/stdout interface without depending on implementation internals.
- Methodology: Category-Partition + Boundary Value Analysis (BVA) + Pairwise Combinatorial Testing + Real-World Workload/Performance Testing.

## Feature Inventory
| # | Feature | Source (requirement) | Tier 1 | Tier 2 | Tier 3 |
|---|---------|----------------------|:------:|:------:|:------:|
| 1 | ROM Scanning & Path Safety | ROM scan, safety path checks | 5 | 5 | ✓ |
| 2 | Header Parsing | Nintendo header parsing & validation | 5 | 5 | ✓ |
| 3 | Dynamic GBC/GBA Resolution | Dynamic resolution resolution (160x144 vs 240x160) | 5 | 5 | ✓ |
| 4 | Controller Button Mappings | Button mappings (incl. GBA L and R) | 5 | 5 | ✓ |
| 5 | Speed Control | Emulation speed scaling (0.5x, 1x, 2x, 4x) | 5 | 5 | ✓ |
| 6 | Frame Skipping | Frame skip logic (skip rendering while advancing state) | 5 | 5 | ✓ |
| 7 | Atomic Savestates | Atomic state serialization and loading | 5 | 5 | ✓ |
| 8 | CPU/APU Execution Mocks | Cycle ticking, audio resampling, syncing | 5 | 5 | ✓ |

## Resource Safety Guidelines
- **Compilation limits**: cargo and cmake build commands must specify at most 2 parallel jobs (e.g. `CARGO_BUILD_JOBS=2` env variable or `-j 2`).
- **Command/spawning execution timeouts**: every external tool call and process spawn within the tests (e.g. executing `mock_emulator.py` or compiled binary) must enforce an explicit timeout parameter (e.g. `timeout=10.0` or similar context-appropriate value).
- **CPU Interpreter cycle/halt checks**: Rust and mock execution loops must count cycles and check for maximum tick thresholds or halting instructions to avoid hangs.

## Test Architecture
- **Test Runner**: Pytest framework, executing tests sequentially or concurrently.
  - Invocation: `pytest tests/test_e2e.py`
  - Pass/Fail: Clean exit code 0 indicates all tests pass.
- **Test Case Format**: 
  - Subprocess runner (`EmulatorProcessRunner`) spawns the emulator binary with specific CLI flags, feeding JSON input files, and capturing video/audio/state dumps for assertion.
  - Interactive runner (`InteractiveEmulatorSession`) communicates with the emulator subprocess over stdin/stdout using text commands, receiving structured responses.
- **Directory Layout**:
  - `tests/mock_emulator.py` - Mock emulator runner script.
  - `tests/test_e2e.py` - E2E Pytest test suite (Tiers 1-4).
  - `tests/test_adversarial.py` - Adversarial validation suite (Tier 5).
  - `roms/` - Contains test ROM files or directory structures for scanning.

## Real-World Application Scenarios (Tier 4)
| # | Scenario | Features Exercised | Complexity |
|---|----------|--------------------|------------|
| 1 | W4.1: Full Boot and Play Game Cycle | Boot, input, play, save/load state | Medium |
| 2 | W4.2: GBA Playthrough with L/R Buttons | GBA mode resolution, L/R button mappings, state verification | Medium |
| 3 | W4.3: Fast-Forward and Skip Mode | Speed scaling, frame skip logic, cycle ticking and audio sizes | High |
| 4 | W4.4: Multi-ROM Session Switcher | Dynamic GBC/GBA reload, state recovery, resolution change | High |
| 5 | W4.5: High-Performance Execution Benchmark | Zero-allocation, performance testing over 10,000 frames | High |

## Coverage Thresholds
- Tier 1: ≥5 per feature (Total: 40 cases)
- Tier 2: ≥5 per feature (Total: 40 cases)
- Tier 3: Pairwise coverage of major feature interactions (Total: 8 cases)
- Tier 4: ≥5 realistic application scenarios (Total: 5 cases)
