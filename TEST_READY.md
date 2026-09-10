# Resultados de validación — 2026-09-10

Validación local en Windows x64 con Rust/Cargo 1.96.1, CMake 4.3.3,
MSVC Build Tools 2022, Python 3.13.14, pytest 9.1.1 y SDL2 2.32.10.
Los cambios de mantenimiento están en el árbol de trabajo, sin publicar.

## Resultado

| Comprobación | Resultado |
|---|---|
| `cargo test --workspace --release --locked -j 2` | 383 correctas; 27 sondas manuales ignoradas |
| Pytest contra `build/bin/Release/clothing_app.exe` | 126 correctas; 1 omitida; 9,15 s en el cierre del runner |
| Reglas de controles C++ | Release y Debug: código de salida 0 |
| Aplicación con Visual Studio | Release y Debug compilan; smoke headless Debug correcto |
| Núcleo con Ninja | Release compila |
| Núcleo con Ninja Multi-Config | Debug y Release compilan |
| Cabeceras CXX con Cargo en caché | Se regeneraron ambas copias retiradas del build; SHA-256 idéntico |
| Frontend sin SDL2 | Configuración rechazada con el error esperado |
| `CARGO_TARGET_DIR` relativo con espacios | Rutas absolutas de Cargo, cabeceras y enlace Debug/Release verificadas en los proyectos generados |

Las 383 pruebas Rust incluyen 340 unitarias y 43 de integración. Dentro de las
unitarias pasaron 111 pruebas JIT y 11 regresiones de persistencia, RTC y ROM;
estos subconjuntos no se suman otra vez al total. La omisión de pytest corresponde
a una prueba de enlaces simbólicos sin permisos disponibles en este Windows.

El runner completo ejecutó correctamente Rust, compilación y controles. Su
primera pasada nativa detectó expectativas heredadas del mock; tras corregirlas,
se repitió la suite nativa completa contra el mismo ejecutable y terminó con
código 0. No hubo cambios de producción después de esa compilación.
El cierre del runner también terminó con código 0 desde otra carpeta usando
`--skip-build --skip-rust-tests`, para reutilizar esas etapas ya verificadas.
Aunque el entorno pedía mock, el runner seleccionó el binario nativo correctamente.

## Correcciones comprobadas

- Dos regresiones JIT fallaron antes del arreglo y pasaron después: continuación
  indebida tras una interrupción Gamecard y reutilización de una entrada ARM
  al ejecutar Thumb. También se cubren bloques encadenados y retiro parcial.
- Los estados GBA conservan fase PPU y canales PSG; GBC conserva HDMA. Los casos
  de compatibilidad verifican la carga de estados anteriores con valores seguros
  para los campos que no existían, sin inventar información perdida.
- RTC avanza en tiempo constante; exploración de ROMs lee como máximo 512 bytes
  por cabecera. Se comprueban límites de lectura y protección de archivos al
  guardar batería GBC, incluido el pie RTC.
- Las pruebas de entrada ejecutan programas sintéticos GBC/GBA y comprueban RAM
  emulada. La restauración recupera el estado del programa, con ranuras por ROM.
- Las exportaciones informan fallos; headless escribe audio por bloques. Se
  compara el audio exportado y se comprueba que exportar no cambia la ejecución.
- El harness exige un ejecutable nativo, salvo selección explícita del mock;
  detecta procesos bloqueados y termina las sesiones. El runner no reescribe tests
  ni instala dependencias durante la validación.

## Mediciones acotadas

Headless sin ROM ni exportación de audio, una ejecución por caso:

| Ticks | Pico de memoria del proceso |
|---|---|
| 200 | 13,180 MiB |
| 20.000 | 13,164 MiB |

La memoria permaneció estable en esta comparación. No es una medición de consumo
con juegos cargados ni una prueba de sesiones gráficas prolongadas.

El benchmark sintético GBA escribe un testigo en EWRAM y entra en HALT. Verifica
10.000 frames, avance de ciclos y el umbral original de menos de 1 ms/tick.
Una medición local dio 0,05248 ms/tick. Mide planificación con CPU detenida;
no acredita esa velocidad con un juego ejecutando instrucciones activamente.

## Límites y evidencia

- La CI Windows está añadida en `.github/workflows/ci.yml`; no se ha publicado
  ni ejecutado remotamente.
- No se ejecutaron las 27 sondas Rust ignoradas, partidas reales, sesiones gráficas
  largas ni una matriz Linux/macOS o de versiones antiguas de Rust.
- El directorio Cargo personalizado se verificó mediante configuración y revisión
  de comandos generados; no se recompilaron allí todas las dependencias.
- Persiste una advertencia previa de paréntesis innecesarios en `jit/block.rs`.
  No afecta al resultado y se excluyó la limpieza cosmética.
- El mapa WRAM NDS existente usa una representación ampliada de 256 KiB; las
  pruebas de asignación no demuestran fidelidad completa a los 32 KiB físicos.

Los registros locales están bajo `build/maintenance-20260910/`, excluidos de Git:
`validation-runner.log`, `validation-entrypoint-final.log`,
`pytest-native-final.log`, `jit-final.log`,
`persistence-lib-final.log`, `native-debug-build.log`, `warm-headers-build.log`,
`ninja-Release-build.log`, `ninja-multi-Debug-build.log`,
`ninja-multi-Release-build.log`, `custom-cargo-target-check.log` y
`headless-memory.json`. `baseline/` conserva los archivos y hashes anteriores
al mantenimiento, incluidos los cambios que ya tenía el usuario.

Para reproducir la validación, consulte [TEST_INFRA.md](TEST_INFRA.md).
