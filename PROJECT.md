# Arquitectura

La aplicación combina un núcleo Rust con un frontend C++17/SDL2. El puente `cxx`
expone estructuras y operaciones desde `core/src/lib.rs`; Cargo genera su código
y encabezados. CMake consume esos mismos encabezados y enlaza la biblioteca estática.

| Carpeta o archivo | Responsabilidad |
|---|---|
| `core/src/emulator.rs` | Carga, estado general, ejecución por ciclos y coordinación de consolas |
| `core/src/gbc/` | CPU GBC, memoria, vídeo, audio y cartuchos MBC3/RTC |
| `core/src/gba/` | ARM7, memoria, vídeo, audio, DMA y Flash GBA |
| `core/src/nds/` | ARM9/ARM7, memoria y periféricos DS, arranque HLE y gráficos |
| `core/src/jit/` | Recompilación Windows x64, cachés, invalidación y pruebas diferenciales |
| `core/src/rom.rs` | Validación de cabeceras, exploración y escritura compartida de batería |
| `core/src/savestate.rs`, `snapshot.rs` | Estados GBC/GBA y snapshots NDS |
| `frontend/src/` | Ventana, entrada, audio, menús y protocolo CLI/interactivo |
| `core/tests/`, `tests/`, `frontend/tests/` | Pruebas Rust, integración por procesos y controles C++ |

## Contratos de mantenimiento

- Los ciclos y efectos de memoria del JIT deben coincidir con el intérprete;
  una interrupción o cambio de ISA exige una transición correcta entre ambos.
- Guardar/restaurar debe preservar la continuación observable. Las ampliaciones
  del estado GBC/GBA son aditivas y siguen admitiendo archivos anteriores.
- El escáner solo necesita la cabecera; la carga del juego lee la ROM completa.
- Los errores de lectura, guardado y exportación deben llegar al llamador.
- El validador prueba el binario real. El mock Python requiere selección explícita.

Véanse [validación](TEST_INFRA.md) y [resultados](TEST_READY.md). El documento
anterior sobre una reparación específica de audio GBA se conserva en
[el historial](docs/history/GBA_AUDIO_RESET.md).
