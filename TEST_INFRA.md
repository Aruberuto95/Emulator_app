# Validación del emulador

## Preparación

Instale las herramientas descritas en [README](README.md) y las dependencias con
`python -m pip install -r requirements-test.txt`. El runner no instala paquetes.

```powershell
.\.venv\Scripts\python.exe run_build_and_test.py
```

El runner resuelve rutas desde el repositorio, admite `--build-dir` y `--config`,
y limita `--jobs` a 1 o 2. Primero ejecuta `cargo test --workspace --locked`, luego
compila la aplicación y el test C++ de controles, y finalmente ejecuta pytest
contra la aplicación compilada. Release usa también el perfil Release de Cargo.

## Comprobaciones separadas

```powershell
cargo test --workspace --release --locked -j 2
.\build\bin\Release\input_mapping_tests.exe
$env:EMULATOR_BIN = (Resolve-Path .\build\bin\Release\clothing_app.exe).Path
.\.venv\Scripts\python.exe -m pytest tests/ -ra
```

`--skip-build` y `--skip-rust-tests` permiten repetir una etapa ya comprobada.
El informe final debe indicar esas omisiones. Un ejecutable ausente genera error;
no hay sustitución automática por el mock.

Para validar exclusivamente el comportamiento del mock:

```powershell
$env:EMULATOR_TEST_BACKEND = "mock"
.\.venv\Scripts\python.exe -m pytest tests/ -ra
Remove-Item Env:EMULATOR_TEST_BACKEND
```

Un resultado del mock no acredita la corrección del núcleo, JIT, vídeo ni audio.

## Cobertura relevante

- Rust: CPU y periféricos, diferencia JIT/intérprete, interrupciones y cambios
  ARM/Thumb, persistencia, RTC, cabeceras y lecturas acotadas.
- C++: reglas de remapeo compartidas con la UI y conservación de asignaciones.
- Pytest nativo: CLI, protocolo interactivo, rutas, estados, controles, audio y
  casos adversariales. Regresiones nuevas cubren errores de exportación, streaming,
  memoria headless y procesos que no responden.

Los walkthroughs de entrada comprueban efectos en RAM de programas GBC/GBA.
El benchmark GBA usa HALT y mide la planificación de periféricos; no representa
el rendimiento de un juego con CPU activa. El audio con ROM usa el reloj de la
consola para calcular el número esperado de muestras.

Las pruebas crean ROMs y partidas sintéticas en directorios temporales.
Las sondas marcadas `#[ignore]` se ejecutan por separado, pueden necesitar ROMs o
estados locales y no forman parte de la validación automática sin datos externos.
No ejecute `cargo test -- --ignored` indiscriminadamente sobre sus partidas.

## Límites y reproducibilidad

Cargo/CMake usan hasta dos trabajos. `EMULATOR_TEST_TIMEOUT` controla los plazos
de intercambio interactivo (10 s por defecto); el runner ofrece
`--build-timeout` (900 s) y `--test-timeout` (180 s) para las etapas completas.
Los procesos bloqueados deben terminar y mostrar sus diagnósticos.

La CI mínima en Windows compila y ejecuta las pruebas sin ROMs externas. Sus
resultados remotos solo existen después de publicar y ejecutar el workflow.
Los resultados locales, pruebas omitidas y limitaciones están en
[TEST_READY.md](TEST_READY.md); los recuentos se obtienen de la ejecución,
no de tablas históricas mantenidas a mano.
