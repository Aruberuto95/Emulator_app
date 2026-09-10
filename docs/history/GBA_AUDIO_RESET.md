> Registro histórico anterior al mantenimiento de septiembre de 2026. Sus cifras y conclusiones no validan la versión actual. Consulte [la guía vigente](../../GUIA_DE_USO.md) y [las validaciones](../../TEST_READY.md).

# Project: GBA Emulator Audio Sound Reset

## Architecture
The Game Boy Advance (GBA) emulator consists of multiple modules, with core memory accesses handled in `core/src/gba/mmu.rs` and audio processing in `core/src/gba/apu.rs`.
- `mmu.rs` manages GBA memory registers, including timer reload registers, timer control registers (Timer 0/1 enabling), and sound control registers (`SOUNDCNT_H`).
- `apu.rs` processes audio, including DirectSound channels A and B which maintain linear-interpolation accumulators (`cycles_since_overflow_a`, `cycles_since_overflow_b`) and periods (`overflow_period_a`, `overflow_period_b`).
- We need to establish a notification mechanism between timer/sound control register modifications in `mmu.rs` and the DirectSound interpolation state in `apu.rs`.

## Milestones
| # | Name | Scope | Dependencies | Status |
|---|---|---|---|---|
| 1 | Exploration & Investigation | Inspect `core/src/gba/apu.rs` and `core/src/gba/mmu.rs` to find timer structures, audio structures, and notification paths. | none | DONE |
| 2 | Implementation | Implement reset notification mechanism and state resets on specific GBA timer/control modifications. | M1 | DONE |
| 3 | Testing & Verification | Add unit tests in `apu.rs` and verify all existing 112 E2E and adversarial tests compile and pass. | M2 | DONE |
| 4 | Forensic Audit | Perform integrity forensic audit to ensure correctness without cheating/fabrication. | M3 | DONE |

## Interface Contracts
### `mmu` ↔ `apu`
- DirectSound linear-interpolation states in `apu.rs` should be exposed or reset via methods.
- Methods to implement on APU: e.g., `reset_ds_a_interpolation()`, `reset_ds_b_interpolation()` or a general notification method.
- Invoked from `mmu.rs` when:
  - Timer 0 or 1 enabled bit transitions from 0 to 1.
  - Timer reload register is written/modified.
  - `SOUNDCNT_H` DirectSound timer selection changes.
  - FIFO reset is commanded.
