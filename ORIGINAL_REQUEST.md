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
