# Original User Request

## Initial Request — 2026-06-29T20:07:32Z

This project designs and develops the core FFI bridge interface using `cxx` for a high-performance multi-console emulator. The backend is written in Rust, and the frontend is written in C++ with SDL2. They communicate via a zero-copy boundary.

Working directory: `/Users/a.rudolph/Proyectos Albert/clothing_app`
Integrity mode: demo

## Requirements

### R1. Hybrid Build Configuration
- Establish a compilation system using **CMake** and **Cargo** directly in the root of the workspace.
- Configure CMake to compile the C++ frontend (with SDL2) and the Rust backend library (using `corrosion` or a custom CMake Cargo target).
- Integrate `cxx` or `cxx-build` to automatically generate the FFI bindings during compilation.

### R2. FFI Bridge Interface (cxx::bridge)
- Create a `cxx::bridge` block in Rust defining the FFI boundaries.
- The interface must expose:
  - Core initialization and destruction.
  - Runtime execution controls: `play`, `pause`, and `reset`.
  - Input injection for controllers (e.g., passing digital button states).
  - Pointers/slices to the video frame buffer (raw pixels) and audio buffer (PCM samples) to allow C++ to read them without intermediate heap copies.

### R3. Rust Core Skeleton
- Implement the Rust crate exposing the FFI functions.
- The core must manage a pre-allocated video frame buffer and audio sample buffer.
- Prevent heap allocations during execution steps.

### R4. C++ Frontend Bootstrap (SDL2)
- Implement a C++ frontend that initializes SDL2 (video, audio, controller inputs).
- On start, initialize the Rust emulator core through the FFI bridge.
- Implement an event loop executing ticks, reading inputs, passing them to the core, and rendering/streaming the zero-copy video/audio frames returned.

## Acceptance Criteria

### Compilation & FFI Security
- [ ] The combined CMake project compiles cleanly on macOS using standard clang/gcc/rustc without warnings.
- [ ] The generated Rust C++ headers are correctly located and included in the C++ sources.
- [ ] No `unsafe` blocks are used for standard FFI boundary data transfers.

### Performance & Zero-Copy Execution
- [ ] The memory addresses of the video buffer retrieved by C++ match the memory address of the pre-allocated buffers on the Rust side, confirming zero-copy access.
- [ ] Visual assets or test patterns (e.g., rendering a gradient or test grid) are written by Rust and drawn on screen by C++ to prove frame buffer continuity.

## Follow-up Request — 2026-06-29T22:09:46Z

Universal Game Boy Color (GBC) and Game Boy Advance (GBA) emulator with a Rust backend and C++ SDL2 frontend using cxx for zero-copy FFI, implemented completely from scratch. The ultimate metric of success is full commercial compatibility and playability for Pokémon Crystal (GBC) and Pokémon Emerald (GBA).

Working directory: /Users/a.rudolph/Proyectos Albert/clothing_app
Integrity mode: development

## Requirements

### R1. Fully Compatible GBC and GBA Emulator Cores from Scratch in Rust
Implement two separate emulator cores in the Rust backend without using external emulation libraries, satisfying the following details:
1. **Game Boy Color (GBC) Core (for Pokémon Crystal):**
   - **CPU:** Complete instruction decoder and interpreter for the Sharp LR35902 CPU, implementing all registers, clock cycle counting, and CPU instruction logic.
   - **Interrupts & Timers:** Precise timing registers (DIV, TIMA, TMA, TAC) and interrupt controller (VBlank, Stat, Timer, Serial, Joypad).
   - **MMU & Mapper:** Support for the **MBC3 Memory Bank Controller**, including ROM banking, RAM banking, and emulation of the Real-Time Clock (RTC) registers.
   - **PPU:** Render tile-based backgrounds, window layer, and sprites (supporting both 8x8 and 8x16 size modes), color palette registers (BCPS, OCPS), and scanline-based timing.
   - **APU:** Audio channels (Square 1 with sweep, Square 2, Wave RAM, Noise) and audio mixing.
   - **SRAM Save:** Emulate battery-backed SRAM writes/reads to save game files.
2. **Game Boy Advance (GBA) Core (for Pokémon Emerald):**
   - **CPU:** Full ARM7TDMI CPU interpreter (ARM and THUMB instruction sets), register mirroring, program status registers (CPSR/SPSR), and pipeline emulation.
   - **BIOS HLE:** High-Level Emulation (HLE) of GBA BIOS system calls (e.g., `CpuSet`, `CpuFastSet`, `RegisterRamReset`, `ArcTan2`, etc.) required by commercial games.
   - **MMU:** Mapped ROM, EWRAM (256KB), IWRAM (32KB), VRAM, OAM, Palette RAM, and dynamic alignment checks.
   - **DMA Controller:** Four DMA channels (DMA0-DMA3) supporting dynamic trigger events (immediate, VBlank, HBlank, Sound FIFO).
   - **PPU:** Render Mode 0 (tiled backgrounds), sprites (OAM attribute decoding, affine transforms/scaling/rotation), palette management, and window masking.
   - **APU & Sound FIFO:** Direct Sound A and B channels with FIFO buffers fed by DMA, alongside legacy Game Boy sound channels.
   - **Backup Memory:** Emulation of **128KB Flash** save file interface (e.g. MX29L010 or similar flash command structures) required by Pokémon Emerald.

### R2. FFI Bridge (cxx) & Frontend Integration
Refactor the FFI boundary (`core/src/lib.rs` and `frontend/src/main.cpp`) to integrate the GBA/GBC cores dynamically:
- Let the C++ frontend load ROM files and pass them through the `cxx` boundary to let Rust inspect/initialize the appropriate core.
- Establish a zero-copy FFI boundary for passing video buffers (GBA resolution: 240x160 RGB24/RGB565, GBC resolution: 160x144 RGB24) and audio buffers (16-bit PCM sound) directly to the C++ frontend.
- C++ frontend must initialize the SDL2 window dynamically adjusting to GBA or GBC resolution, and poll inputs via SDL events mapping them to GBC/GBA digital buttons.

### R3. Safe ROM Scanning and Header Parsing
- The Rust core must scan the `./roms` directory, identifying `.gb`, `.gbc`, and `.gba` extensions.
- Implement header decoding and checksum verification (Nintendo Logo verification, Global Checksum, header checksum) to validate the ROM before instantiating a core.
- Apply strict path sanitization to prevent Path Traversal attacks (OWASP compliance) and enforce limits on ROM buffer sizing to prevent memory exhaustion and out-of-bounds reads.

### R4. Speed Control, Frame Skipping, and Audio Resampling
- Implement FFI control for speed multipliers: `0.5x`, `1.0x`, `2.0x`, `4.0x`.
- Rust must scale the number of emulation ticks run per video frame.
- Implement frame skipping for speeds > 1.0x (skipping PPU pixel generation while keeping CPU, DMA, timers, and game logic ticking).
- Resample the audio output dynamically (using linear interpolation, decimation, or fractional resampling) to match the real-time sample rate required by SDL2, preventing audio underflow, buffer overflow, and crackling/clicking.

### R5. Atomic Multisesion Savestates
- Serialize and deserialize the complete emulator state (CPU registers, MMU, internal RAM, PPU, and timers).
- State saving must be atomic: serialize to a temporary file (e.g. `slot0.state.tmp`), perform disk synchronization (flush), and rename it to the final state file (`slot0.state`) to prevent corruption in case of crash or disk exhaustion.

## Acceptance Criteria

### Compilation & Compatibility
- [ ] The hybrid build compiles successfully: `cmake -B build -S .` and `cmake --build build` run without errors.
- [ ] All existing E2E and adversarial tests (run via `pytest tests/`) continue to pass.
- [ ] Added unit and integration tests verifying GBC/GBA instruction interpreters.
- [ ] **Pokémon Crystal Compatibility:** The emulator successfully loads, boots past the game check screens, displays the opening animation and title screen, renders gameplay graphics, accepts keypad inputs, plays audio, and saves/loads progress.
- [ ] **Pokémon Emerald Compatibility:** The GBA core correctly handles BIOS HLE calls, boots Pokémon Emerald, executes title sequences, renders sprite and tile graphics in Mode 0, processes Flash save reads/writes, and plays audio through the Direct Sound FIFO channels.
- [ ] Zero heap allocations occur in the active execution loop (`tick()`).
- [ ] All path traversal tests and overflow checks pass successfully.

## Follow-up Request — 2026-06-29T22:28:10Z

añade seguros al uso de ram para no saturar el computador si un compilador falla y queda en un loop infinito que afecte la ram del computador

Para cumplir con esto, implementa e integra las siguientes medidas de seguridad de recursos en las actividades del equipo:
1. Restricción de Concurrencia de Compilación:
   - Configura la variable de entorno `CARGO_BUILD_JOBS=2` o compila usando el flag `-j 2` (por ejemplo, `cargo build -j 2` y `cmake --build build -j 2`). Esto limitará los hilos simultáneos de rustc/clang y evitará picos extremos de uso de memoria RAM.
2. Timeouts Estrictos en Comandos de Ejecución y Testeo:
   - Todos los comandos de compilación, tests (`pytest`), o subprocesses del emulador deben ejecutarse con límites de tiempo (timeouts) explícitos (ej. en Python usando `timeout` en `subprocess.run`).
3. Salvaguardas en los Intérpretes de CPU:
   - Diseña los loops de ejecución de los cores de CPU de Rust (LR35902 y ARM7TDMI) con contadores de ciclo límite y manejo de excepciones para evitar que instrucciones corruptas causen bucles infinitos y fugas de memoria.

## Follow-up Request — 2026-06-29T22:43:47Z

El servidor se ha reiniciado de nuevo. Causa raíz de la saturación de RAM y congelamiento durante las pruebas resuelta:
1. Causa Raíz de Saturación:
   - El mock emulator (`tests/mock_emulator.py`) leía archivos sin límites de tamaño usando `json.load(f)`. Al ejecutar la prueba adversarial `test_interactive_inject_infinite_stream_dos`, intentaba leer indefinidamente del flujo infinito `/dev/urandom`, lo cual llenaba la RAM del sistema.
   - Además, existía un bloqueo por buffering (deadlock) porque en caso de error en la inyección de archivos grandes se llamaba a `continue` sin limpiar/vaciar el búfer de salida (`sys.stdout.flush()`), lo que dejaba colgado al test runner esperando respuesta.

2. Solución Implementada:
   - Modificado `tests/mock_emulator.py` forzando un límite estricto de lectura de 1MB (`f.read(1024 * 1024 + 1)`) en todas las cargas de inyección (`args.input_inject` y comando `INJECT`) y savestates (`load_state`).
   - Añadido `sys.stdout.flush()` explícito antes de cualquier `continue` en el bucle interactivo del mock.
   - Agregado una validación de rango de frame (`frame > 1000000`) para simular correctamente el control de desbordamientos de enteros de la arquitectura real.
   - Alineado las comprobaciones de rutas de comandos `DUMP_*` en el mock para verificar contra `ALLOWED_DUMP_DIR` en lugar de la ruta raíz general.

Todos los 99 tests de la suite pasan ahora perfectamente en menos de 8 segundos sin ningún tipo de saturación de memoria o bloqueos. Continuar con los hitos de implementación de los cores reales.

## Follow-up Request — 2026-06-30T00:08:38Z

El usuario ha realizado modificaciones locales en `tests/mock_emulator.py` para soportar de manera no-destructiva campos personalizados en la carga/guardado de savestates (añadiendo `self.extra_fields` para capturar cualquier campo que no preocupe a `required_keys` y devolverlo al serializar el estado).
Todos los 105 tests pasan de forma exitosa en 7.41 segundos. Continuar con el proceso de hardenización (Hito M7) teniendo en cuenta este cambio.

## Follow-up Request — 2026-06-30T00:16:56Z

El usuario ha realizado modificaciones locales adicionales en `tests/mock_emulator.py` para asegurar que los campos personalizados sean persistidos correctamente al guardar el estado (`state_data.update(self.extra_fields)` en `save_state`), y ha incrementado el límite de tamaño de savestate de 1MB a 2MB en `load_state`.
Todos los 112 tests pasan limpiamente en 8.52 segundos. Continuar con las tareas de hardenización (Hito M7) considerando que el límite para archivos de savestate es ahora de 2MB.
