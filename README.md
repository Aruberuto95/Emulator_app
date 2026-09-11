# Emulator_app — guía de uso

Emulador de Game Boy/Game Boy Color, Game Boy Advance y Nintendo DS, con núcleo
Rust y aplicación C++17/SDL2. NDS incluye arranque HLE, doble pantalla, audio y
entrada táctil; la compatibilidad depende del juego. El JIT NDS funciona en
Windows x64; los demás hosts usan el intérprete.

Esta es la documentación única del repositorio. Instrucciones contrastadas con
las fuentes el 11 de septiembre de 2026. Los resultados de ejecución citados
corresponden al 10 de septiembre; no representan una nueva validación.

## Índice

- [Instalar y compilar](#instalar-y-compilar)
- [Abrir juegos y usar los controles](#abrir-juegos-y-usar-los-controles)
- [Guardar y recuperar partidas](#guardar-y-recuperar-partidas)
- [Resolver problemas](#resolver-problemas)
- [Usar la CLI y el modo sin ventana](#usar-la-cli-y-el-modo-sin-ventana)
- [Validar cambios](#validar-cambios)
- [Medir rendimiento NDS](#medir-rendimiento-nds)
- [Compilar con PGO](#compilar-con-pgo)
- [Arquitectura y mantenimiento](#arquitectura-y-mantenimiento)
- [Resultados registrados y pendientes](#resultados-registrados-y-pendientes)

## Instalar y compilar

### Requisitos en Windows

- Rust/Cargo en PATH. El núcleo declara Rust **1.85** como mínimo; la configuración
  registrada y la CI usan **1.96.1**. No hay una matriz validada de versiones anteriores.
- CMake **3.20+** y MSVC Build Tools con C++ y Windows SDK.
- Python **3.10+** para instalación y pruebas.
- SDL2 **2.32.10**, instalado con el script del repositorio.

El entorno de la última validación usó Windows x64, MSVC Build Tools 2022,
CMake 4.3.3, Python 3.13.14 y pytest 9.1.1. Los comandos siguientes se ejecutan
en PowerShell desde la raíz del repositorio.

### Primera instalación

~~~powershell
python -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements-test.txt
.\.venv\Scripts\python.exe install_sdl2.py --download-only
.\.venv\Scripts\python.exe run_build_and_test.py
.\clothing_app.exe
~~~

Compruebe que cada comando termine correctamente antes de continuar.
El instalador verifica el SHA-256 del archivo descargado de SDL2. El validador
ejecuta las pruebas Rust, compila Release con hasta dos trabajos, ejecuta los
tests C++ de controles y cadencia, y prueba el ejecutable real con pytest.
No instala dependencias ni reescribe las fuentes de las pruebas.

### Actualizar o compilar sin pruebas

Cierre la aplicación y ejecute:

~~~powershell
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DSDL2_DIR=sdl2/SDL2-2.32.10/cmake
cmake --build build --config Release --target clothing_app --parallel 2
.\clothing_app.exe
~~~

Con Visual Studio, el ejecutable queda en
`build/bin/Release/clothing_app.exe`. CMake también copia la aplicación a la raíz
y, con el SDK compartido indicado, coloca `SDL2.dll` junto a ambos ejecutables.
La compilación Debug también actualiza la copia de la raíz: use
`--config Release` para dejar allí la versión para jugar.

La compilación incremental basta; no es necesario borrar `build/` ni las cachés.
`install_sdl2.py` sin `--download-only` instala SDL2 y compila Release;
`--build-dir` permite elegir otro directorio. Si CMake conserva un SDK anterior,
ejecute ese instalador completo o vuelva a configurar `SDL2_DIR` como arriba.

Para compilar únicamente el núcleo:

~~~powershell
cargo build --release --locked -j 2
~~~

También se puede configurar CMake con `-DEMULATOR_BUILD_FRONTEND=OFF`.
Una compilación del frontend sin SDL2 falla explícitamente.
En Linux/macOS, instale SDL2 con el gestor del sistema; el instalador Python es
exclusivo de Windows. Esas plataformas no se validaron en la campaña registrada.

## Abrir juegos y usar los controles

~~~powershell
.\clothing_app.exe
.\clothing_app.exe --rom "ruta\juego.nds"
~~~

Sin `--rom` se abre el explorador, inicialmente en `roms/` al ejecutar desde
la raíz. Admite `.gb`, `.gbc`, `.gba` y `.nds`. Arriba/abajo seleccionan,
Enter o Espacio abren y Backspace sube de carpeta.

| Acción | Control predeterminado |
| --- | --- |
| Cruceta | Flechas |
| A / B | A / S |
| L / R | Q / W |
| Start / Select | Enter / C |
| X / Y de NDS | X / Y |
| Pantalla táctil NDS | Clic o arrastre izquierdo en la pantalla inferior |
| Ajustes y pausa | Escape |
| Menú de estados | F2 |
| Elegir ranura activa durante el juego | 0–9 |
| Guardar / cargar la ranura activa | F5 / F9 |
| Solicitar reinicio | Ctrl+R |

En ajustes, arriba/abajo seleccionan una fila. Para reasignar un botón, pulse
Enter/Espacio y luego la nueva tecla; Escape cancela. Las teclas duplicadas o
reservadas se rechazan conservando la asignación anterior.

| Ajuste | Uso |
| --- | --- |
| SPEED | Izquierda/derecha: de 0.5x a 5x, en pasos de 0.1x |
| FRAME SKIP | Izquierda/derecha: 0–9; aparece después de SPEED |
| SCALE | Izquierda/derecha: escala de la ventana |
| VSYNC | Izquierda/derecha o Enter/Espacio: activar/desactivar |
| RESTART / EXIT | Enter/Espacio: solicitar reinicio o volver al explorador |

La velocidad seleccionada es una petición: el resultado depende del juego,
la escena y el equipo. FRAME SKIP 0 evita la omisión manual, pero NDS mantiene
la omisión adaptativa de composiciones atrasadas desde 2x. CPU, audio y periféricos
siguen avanzando. Desactivar VSync puede reducir esperas, a costa de cortes
visibles en la imagen.

## Guardar y recuperar partidas

### Estados rápidos

F2 abre el menú: arriba/abajo eligen ranura; izquierda/derecha/Tab alternan
guardar/cargar; Enter/Espacio confirman; Escape o F2 cierran. Durante el juego,
0–9 seleccionan la ranura usada por F5/F9.

En Windows, los ajustes y estados se guardan normalmente en:

~~~text
%APPDATA%\EmulatorApp\EmulatorApp\
~~~

SDL determina la carpeta del usuario; si no está disponible, la aplicación
intenta usar la del ejecutable y, finalmente, el directorio actual.

Los estados nuevos tienen el nombre
`<nombre>_<huella-ROM>_savestate_<ranura>.sav`.
La huella se calcula sobre el contenido completo de la ROM: dos juegos con el
mismo nombre usan ranuras distintas. La carga verifica identidad e integridad
antes de aplicar el estado.

GBC/GBA usan JSON. NDS escribe un contenedor binario **v4** y admite v2/v3;
los estados actuales conservan créditos de reloj y trabajo gráfico pendiente.
No se garantiza que un lector antiguo pueda abrir archivos nuevos.

### Compatibilidad con partidas anteriores

Todavía se buscan archivos `<nombre>_savestate_<ranura>.sav`.
Los genéricos `savestate_<ranura>.sav` no se asignan automáticamente a una ROM:
para recuperar uno, haga una copia con el nombre antiguo de la ROM correcta.

Los estados anteriores carecen de la huella completa y pueden omitir fases de
audio/vídeo, transferencias o trabajo 3D. La carga admite los formatos compatibles,
pero no puede reconstruir información que nunca se guardó. No reescribe el
archivo antiguo al cargarlo. Conserve una copia antes de migrar o sustituir estados.
Las capturas diagnósticas `.snap` antiguas sin cabecera deben regenerarse;
son distintas de los estados `.sav` compatibles.

### Guardado interno del juego

El guardado desde el menú del juego usa batería/SRAM/Flash y se escribe junto a
la ROM como `<nombre>.sav`. Es distinto del estado rápido. Se escribe
periódicamente, al salir y al cambiar de juego. Cargar un estado rápido deja
pendiente la escritura de su batería: al cerrar persistirá la partida restaurada.

Si falla una escritura, los datos quedan pendientes y aparece un error. Si ocurre
al cerrar o volver al explorador, el juego permanece abierto para permitir
reintentar. Libere espacio o restablezca el acceso a la carpeta de la ROM.
Headless devuelve un código distinto de cero; el protocolo interactivo ofrece
`FLUSH_BATTERY` para intentar la escritura explícitamente.

| Consola | Backend de guardado actual | Validación de batería del 10-09-2026 |
| --- | --- | --- |
| GBC | MBC3/SRAM con RTC | Crystal: guardar, cerrar y continuar correcto |
| GBA | Flash de 128 KiB | Emerald: mismo ciclo correcto; persiste el aviso de reloj agotado |
| NDS | Flash de 512 KiB | SoulSilver: escritura parcial corregida; al continuar aparece «Communication error» |

Esto no acredita todos los cartuchos ni SRAM/EEPROM GBA u otros chips NDS.
Una batería ya corrupta no se repara automáticamente: recupere una copia o un
estado que aún contenga la partida intacta y vuelva a guardar desde el juego.

## Resolver problemas

| Síntoma | Comprobación o acción |
| --- | --- |
| Cargo o CMake no se encuentran | Revise PATH; el runner también busca sus ubicaciones habituales en Windows |
| CMake no encuentra SDL2 | Ejecute el instalador o configure `SDL2_DIR` con la ruta de la primera instalación |
| El instalador informa SDK incompleto | Mueva esa carpeta de SDK a una copia de respaldo y repita la instalación |
| Falta `SDL2.dll` | Mantenga la DLL del SDK usado junto al ejecutable |
| La compilación no puede reemplazar el ejecutable | Cierre la instancia abierta y repita el build |
| Avance rápido lento o comportamiento de una versión anterior | Compile Release y abra la copia recién actualizada; compruebe la velocidad real |
| «KEY ALREADY IN USE» / «KEY RESERVED» | Elija otra tecla; el mapeo anterior permanece intacto |
| No aparece una partida rápida | Revise ROM, ranura, carpeta y formato de nombre; vea compatibilidad anterior |
| Error al guardar o cerrar | Compruebe permisos y espacio en la carpeta de la ROM; vuelva a intentar |
| SoulSilver muestra «Communication error» al continuar | Limitación pendiente observada con ambos JIT activados y desactivados |
| Sprites corruptos después de combate NDS | Use una compilación actual y un estado anterior al combate; un estado ya corrupto no se repara al bajar a 1x |

El intervalo NDS actual es **2048 ciclos de bus**. Las instrucciones históricas
que recomendaban 4096 como valor vigente quedaron obsoletas tras la corrección
de sincronización. Quite cualquier ajuste experimental de `EMU_NDS_SLICE`
para comprobar el comportamiento predeterminado.

## Usar la CLI y el modo sin ventana

~~~powershell
.\clothing_app.exe --headless --rom "ruta\juego.gba" --play --ticks 120
.\clothing_app.exe --rom "ruta\juego.nds" --load-state 0 --speed 2 --frame-skip 0 --play
.\clothing_app.exe --interactive
~~~

| Opción | Función |
| --- | --- |
| `--rom <ruta>` | Cargar una ROM |
| `--headless` | Ejecutar sin ventana |
| `--play`, `--pause`, `--reset` | Controlar el estado inicial de ejecución |
| `--ticks <entero>` | Cantidad de ticks en headless; de 0 a 2147483647 |
| `--speed <valor>` | Velocidad solicitada dentro del intervalo admitido por el núcleo |
| `--frame-skip <0–9>` | Omisión manual de frames |
| `--load-state <ranura>` | Restaurar un estado; requiere ROM y confirma `LOAD_STATE_OK` |
| `--input-inject <JSON o archivo>` | Inyectar botones o una secuencia para pruebas |
| `--dump-video <ruta>` | Exportar el framebuffer RGB888 sin cabecera |
| `--dump-audio <ruta>` | Exportar PCM estéreo de 16 bits, sin cabecera WAV |
| `--dump-state <ruta>` | Exportar información diagnóstica; no sustituye a guardar una ranura |
| `--interactive` | Leer comandos de texto por stdin |

Una restauración inválida termina antes de abrir los dispositivos; no se sustituye
por un arranque nuevo. Speed/frame skip indicados explícitamente prevalecen sobre
los ajustes del estado restaurado. Headless escribe audio por bloques sólo cuando
se solicita; una exportación fallida devuelve un código de salida no nulo.

El protocolo interactivo admite, entre otros, `LOAD_ROM <ruta>`, `PLAY`, `PAUSE`,
`TICK`, `INJECT <JSON>`, `SET_SPEED <valor>`, `SET_FRAME_SKIP <valor>`,
`SAVE_STATE <ranura>`, `LOAD_STATE <ranura>`, `FLUSH_BATTERY` y los comandos
`DUMP_STATE`, `DUMP_VIDEO`, `DUMP_AUDIO` con una ruta. Envíe un comando por línea,
sin BOM, y compruebe la respuesta. El saludo heredado `MOCK_EMULATOR_READY`
también lo emite el ejecutable nativo; no identifica el backend utilizado.

Para aislar estados durante una prueba:

~~~powershell
$env:ALLOWED_DUMP_DIR = (Resolve-Path "ruta\copias-de-prueba").Path
.\clothing_app.exe --rom "ruta\copias-de-prueba\juego.nds" --load-state 0 --play
Remove-Item Env:ALLOWED_DUMP_DIR
~~~

`ALLOWED_DUMP_DIR` cambia la carpeta de estados de GUI/CLI/protocolo y restringe
los destinos `DUMP_*` del protocolo interactivo. Las opciones headless
`--dump-*` escriben en la ruta indicada. La variable no mueve los ajustes ni
la batería: para aislar ésta debe copiar también la ROM y su `.sav`.
`EMU_STATE_DIR` y `EMU_STATE_SLOT` sólo seleccionan estados en sondas Rust.

## Validar cambios

El comando de validación completa es el de [primera instalación](#primera-instalación).
Su implementación está en [run_build_and_test.py](run_build_and_test.py).

| Parámetro del runner | Uso |
| --- | --- |
| `--build-dir <ruta>` | Directorio de compilación; relativo a la raíz si no es absoluto |
| `--config <configuración>` | Release por defecto; admite Debug, RelWithDebInfo y MinSizeRel |
| `--jobs <1 o 2>` | Paralelismo; 2 por defecto |
| `--build-timeout <segundos>` | Plazo de cada etapa de compilación/Rust; 900 por defecto |
| `--test-timeout <segundos>` | Plazo de cada etapa C++/pytest; 180 por defecto |
| `--skip-build`, `--skip-rust-tests` | Reutilizar etapas ya comprobadas; declarar las omisiones al informar resultados |
| `--emulator-bin <ruta>` | Elegir el binario nativo; los dos tests C++ deben estar en su misma carpeta |

Para ejecutar etapas por separado después de compilarlas:

~~~powershell
cargo test --workspace --release --locked -j 2
.\build\bin\Release\input_mapping_tests.exe
.\build\bin\Release\frame_pacing_tests.exe
$env:EMULATOR_TEST_BACKEND = "native"
$env:EMULATOR_BIN = (Resolve-Path ".\build\bin\Release\clothing_app.exe").Path
.\.venv\Scripts\python.exe -m pytest tests/ -ra
Remove-Item Env:EMULATOR_TEST_BACKEND, Env:EMULATOR_BIN
~~~

El runner exige el ejecutable real y selecciona `native` explícitamente.
Para probar sólo el mock, ejecute pytest directamente con
`EMULATOR_TEST_BACKEND=mock` y quite después esa variable.
Ese resultado no valida CPU, JIT, vídeo ni audio del emulador.
`EMULATOR_TEST_TIMEOUT` controla los intercambios interactivos del harness
(10 segundos por defecto), aparte de los plazos del runner.

La cobertura incluye CPU/periféricos, paridad JIT/intérprete, cambios ARM/Thumb,
persistencia e identidad de ROM, RTC, controles, cadencia, CLI, exportación y
recuperación de errores. Las pruebas normales crean ROMs sintéticas y archivos
temporales; no necesitan las ROMs ni partidas del usuario.

Las sondas Rust `#[ignore]` se ejecutan individualmente y pueden necesitar datos
locales. No ejecute todas las ignoradas sobre sus partidas originales.
Un benchmark GBA que entra en HALT mide planificación con CPU detenida, no el
rendimiento de un juego activo. PCM no vacío tampoco demuestra audio audible.

La [CI Windows](.github/workflows/ci.yml) ejecuta el runner sin ROMs externas,
con dispositivos SDL ficticios. Su definición local no demuestra que un workflow
remoto haya pasado; consulte el resultado de la ejecución correspondiente.

## Medir rendimiento NDS

### JIT y sincronización

ARM9 y ARM7 tienen JIT activo por defecto en Windows x64; GBC/GBA no usan estos
recompiladores. ARM9 permite encadenamiento y despacho indirecto; ARM7 ejecuta
bloques sin encadenamiento por defecto. Para comparar con el intérprete:

~~~powershell
$env:EMU_ARM9_JIT = "0"
$env:EMU_ARM7_JIT = "0"
.\clothing_app.exe
Remove-Item Env:EMU_ARM9_JIT, Env:EMU_ARM7_JIT
~~~

`EMU_ARM9_JIT_THUMB=1` y `EMU_ARM7_JIT_THUMB=1` activan compilación Thumb
experimental. Consulte [core/src/jit/mod.rs](core/src/jit/mod.rs) para los contratos.
`EMU_NDS_SLICE` es un ajuste diagnóstico, leído una vez y limitado a 8–8192;
el valor predeterminado de 2048 ciclos de bus preserva la relación
ARM9 : ARM7 : bus = 2 : 1 : 1.

### Medición reproducible

Use copias de ROM, batería y estados; registre escena, hash del ejecutable,
toolchain, equipo, energía, VSync y audio. Compare ejecuciones consecutivas sin
compilar ni correr otra prueba de CPU a la vez.

~~~text
segundos_emulados = delta(cpu_cycles) / 33_513_982
velocidad_real = segundos_emulados / segundos_monotónicos
frames_PPU ≈ delta(cpu_cycles) / 560_190
~~~

Los FPS de presentación y la velocidad solicitada no prueban el avance emulado.
Para sesiones GUI, prepare una carpeta con copias y una ranura válida:

~~~powershell
$run = (Resolve-Path "ruta\copias-de-prueba").Path
$rom = Join-Path $run "juego.nds"
$env:ALLOWED_DUMP_DIR = $run
$env:EMU_SPEED_STATS = "1"
$env:EMU_AUDIO_STATS = "1"
$env:EMU_BENCH_SECONDS = "300"
.\clothing_app.exe --rom $rom --load-state 0 --speed 5 --frame-skip 0 --play 2> "$run/gui.log"
Remove-Item Env:ALLOWED_DUMP_DIR, Env:EMU_SPEED_STATS, Env:EMU_AUDIO_STATS, Env:EMU_BENCH_SECONDS
~~~

`EMU_SPEED_STATS` se activa por presencia, incluso con valor `0`.
Emite por stderr ventanas de aproximadamente 0,5 s con `requested`, `achieved`,
`emulated_s`, `wall_s`, `core_s`, `ticks_count`, `loop_fps` y `vsync`.
En NDS desde 2x añade `composed_fps` y `max_video_gap_ms`: miden composición,
no imágenes distintas ni presentación física. Pausas, restauraciones y cambios
de velocidad reinician las ventanas; puede omitirse el fragmento final.
Agregue `sum(emulated_s) / sum(wall_s)`, sin promediar multiplicadores.
`EMU_BENCH_SECONDS` limita sólo la GUI, incluyendo pausas.

Repita exterior, interior y combate, con transición 1x→5x→1x, movimiento, salida
del combate, guardar/cargar y revisión audiovisual. Audio ficticio o menor
frecuencia son diagnósticos separados.

La sonda `wall_clock_speed_ceiling_probe` mide el núcleo sin presentación GUI:

~~~powershell
cmake -E env "EMU_BENCH_ROM=$rom" "EMU_STATE_DIR=$run" EMU_STATE_SLOT=0 EMU_BENCH_SPEEDS=1,5 EMU_BENCH_WINDOW_MS=8000 EMU_BENCH_REPEATS=3 EMU_BENCH_FRAME_SKIP=0 EMU_BENCH_AUDIO_HZ=48000 cargo test --locked --release -j 2 -p emulator_core wall_clock_speed_ceiling_probe -- --ignored --nocapture --test-threads=1
~~~

| Variable de sonda | Valores / predeterminado |
| --- | --- |
| `EMU_BENCH_ROM` | ROM NDS; ausente mantiene el barrido de tres consolas |
| `EMU_STATE_DIR`, `EMU_STATE_SLOT` | Escena; ranura `0` por defecto, sin directorio mide arranque |
| `EMU_BENCH_WINDOW_MS` | 1–300000; 1500 por defecto |
| `EMU_BENCH_REPEATS` | 1–20; 3 por defecto |
| `EMU_BENCH_SPEEDS` | Lista admitida por el núcleo; `1,4,5` por defecto |
| `EMU_BENCH_FRAME_SKIP` | 0–9; ausente conserva el estado |
| `EMU_BENCH_AUDIO_HZ` | 8000–192000; NDS usa 48000 por defecto |

La sonda recarga cada muestra y calienta aproximadamente un segundo emulado.
Falla si una escena explícita no carga. `nds_interpreter_cost_probe` desglosa
CPU/3D/2D/resto con 30 ticks de calentamiento y 120 medidos a 1x/5x; comparte
ROM/estado/audio/frame skip, pero no ventana/repeticiones/speeds.
Su instrumentación añade coste y un resultado `SKIP` no cuenta como aprobación.

## Compilar con PGO

PGO es opcional; la compilación normal no lo activa. El flujo conservado usa
Visual Studio 2022 x64 y `x86_64-pc-windows-msvc`, sin `target-cpu=native`.
Use el mismo checkout y toolchain para instrumentar, entrenar y compilar el
candidato. Regenerar perfiles al cambiar fuentes evita desajustes de funciones.

Ejecute las etapas en una consola dedicada, comprobando cada código de salida.
Las rutas de PGO deben carecer de espacios. Prepare una carpeta con **copias**
de una ROM NDS, al menos una GBA y una GBC, sus baterías y estados NDS en las
ranuras `0`, `room` y `battle` (exterior, interior y combate). Adapte el nombre
de la ROM siguiente; los fixtures no se distribuyen con el repositorio.

### Instrumentar

~~~powershell
$repo = (Get-Location).Path
$pgo = Join-Path $repo ('build/pgo-' + [guid]::NewGuid().ToString('N'))
$fixtures = 'C:/ruta/copias-pgo'
$nds = Join-Path $fixtures 'juego.nds'
Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
rustc -Vv
rustup component add llvm-tools-preview
$rustSysroot = (rustc --print sysroot).Trim()
$rustLib = Join-Path $rustSysroot 'lib/rustlib/x86_64-pc-windows-msvc'
$profdataTool = Join-Path $rustLib 'bin/llvm-profdata.exe'
$profiler = (Get-ChildItem -LiteralPath "$rustLib/lib" -Filter 'libprofiler_builtins-*.rlib' | Select-Object -First 1).FullName.Replace('\', '/')
New-Item -ItemType Directory -Path "$pgo/build-raw" | Out-Null
$env:LLVM_PROFILE_FILE = "$pgo/build-raw/%m-%p.profraw"
$env:RUSTFLAGS = "-Cprofile-generate=$pgo/build-raw"
cmake -S . -B "$pgo/generate" -G 'Visual Studio 17 2022' -A x64 "-DRUST_TARGET_DIR=$pgo/generate-cargo" "-DCMAKE_EXE_LINKER_FLAGS=$profiler"
cmake --build "$pgo/generate" --config Release --target clothing_app --parallel 2
~~~

El enlace C++ necesita `libprofiler_builtins-*.rlib` del mismo toolchain;
no use `/WHOLEARCHIVE`. No hace falta esa biblioteca explícita en el build final.
CMake copia también esta aplicación instrumentada a la raíz: complete el build
final antes de volver a usar la copia de la raíz para jugar o medir.

### Entrenar

Cree el directorio de entrenamiento después de compilar para excluir perfiles
de build scripts. `%m-%p` separa módulos y procesos.

~~~powershell
$raw = Join-Path $pgo 'training-raw'
New-Item -ItemType Directory -Path $raw | Out-Null
$env:LLVM_PROFILE_FILE = "$raw/%m-%p.profraw"
$env:ALLOWED_DUMP_DIR = $fixtures
$train = "$pgo/generate/bin/Release/clothing_app.exe"
foreach ($slot in '0', 'room', 'battle') {
    foreach ($speed in 1, 5) {
        & $train --headless --rom $nds --load-state $slot --speed $speed --frame-skip 0 --ticks 600 --play
        if ($LASTEXITCODE -ne 0) { throw "Falló entrenamiento DS: $slot / $speed" }
    }
}
foreach ($romCopy in Get-ChildItem -LiteralPath $fixtures -File | Where-Object Extension -In '.gba', '.gbc') {
    foreach ($speed in 1, 5) {
        & $train --headless --rom $romCopy.FullName --speed $speed --ticks 600 --play
        if ($LASTEXITCODE -ne 0) { throw 'Falló entrenamiento GBA/GBC' }
    }
}
$env:EMU_BENCH_SECONDS = '30'
& $train --rom $nds --load-state 0 --speed 5 --frame-skip 0 --play
if ($LASTEXITCODE -ne 0) { throw 'Falló entrenamiento GUI' }
Remove-Item Env:EMU_BENCH_SECONDS, Env:ALLOWED_DUMP_DIR
~~~

Exija `LOAD_STATE_OK`, las tres consolas y perfiles raw no vacíos.
Entrene también movimiento y salida de combate en GUI; headless no ejercita
su recuperación con ticks sin vídeo. Deje cerrar normalmente los procesos para
escribir los perfiles. El instrumentado no sirve para medir rendimiento.

### Fusionar y construir el candidato

~~~powershell
$profile = Join-Path $pgo 'nds.profdata'
& $profdataTool merge -o $profile $raw
if ($LASTEXITCODE -ne 0) { throw 'Falló llvm-profdata merge' }
Remove-Item Env:LLVM_PROFILE_FILE
$env:RUSTFLAGS = "-Cprofile-use=$profile -C llvm-args=-pgo-warn-missing-function"
cmake -S . -B "$pgo/use" -G 'Visual Studio 17 2022' -A x64 "-DRUST_TARGET_DIR=$pgo/use-cargo"
cmake --build "$pgo/use" --config Release --target clothing_app --parallel 2
Remove-Item Env:RUSTFLAGS
~~~

El candidato queda en `$pgo/use/bin/Release/clothing_app.exe`.
Conserve perfiles, hashes, fuentes y flags; compruebe los avisos de perfil.
Valide ese binario mediante [las pruebas nativas](#validar-cambios) y
[las escenas de rendimiento](#medir-rendimiento-nds), sin sustituirlo por otro
build durante la comparación. Una compilación normal posterior vuelve a generar
la aplicación sin PGO.

## Arquitectura y mantenimiento

Cargo genera el puente `cxx` desde `core/src/lib.rs`; CMake consume sus
cabeceras y enlaza la biblioteca estática Rust con el frontend SDL2.

| Ruta | Responsabilidad |
| --- | --- |
| [core/src/emulator.rs](core/src/emulator.rs) | Carga, ejecución por ciclos y coordinación de consolas |
| [core/src/gbc/](core/src/gbc/) | CPU, memoria, vídeo, audio y cartuchos MBC3/RTC |
| [core/src/gba/](core/src/gba/) | ARM7, memoria, vídeo, audio, DMA y Flash |
| [core/src/nds/](core/src/nds/) | ARM9/ARM7, MMU, IPC, periféricos, arranque HLE y gráficos |
| [core/src/jit/](core/src/jit/) | Recompilación Windows x64, cachés, invalidación y pruebas diferenciales |
| [core/src/rom.rs](core/src/rom.rs) | Cabeceras, exploración y escritura compartida de batería |
| [core/src/savestate.rs](core/src/savestate.rs), [snapshot.rs](core/src/snapshot.rs) | Persistencia GBC/GBA y contenedores NDS |
| [frontend/src/](frontend/src/) | Ventana, entrada, audio, menús y CLI/protocolo interactivo |
| [core/tests/](core/tests/), [tests/](tests/), [frontend/tests/](frontend/tests/) | Pruebas Rust, integración por procesos y regresiones C++ |

Los ciclos y efectos de memoria del JIT deben coincidir con el intérprete,
incluidas interrupciones y cambios de ISA. Guardar/restaurar debe preservar la
continuación observable y mantener compatibilidad explícita con estados anteriores.
Los errores de lectura, guardado y exportación deben llegar al llamador.
El escáner sólo necesita cabeceras; cargar el juego requiere la ROM completa.

Actualice esta guía cuando cambien comandos, controles, formatos o limitaciones.
Obtenga recuentos de pruebas y rendimiento de ejecuciones identificadas; registre
binario, condiciones y omisiones. Las antiguas guías, informes y notas de
coordinación se consolidaron aquí; su detalle permanece en el historial de Git.

## Resultados registrados y pendientes

Estos son antecedentes del **10-09-2026**, no pruebas ejecutadas durante la
consolidación documental del 11-09-2026.

| Comprobación registrada | Resultado y alcance |
| --- | --- |
| Rust workspace Release | 398 aprobadas; 27 sondas manuales ignoradas |
| Pytest nativo | 159 aprobadas; 1 omitida por permisos de enlaces simbólicos; incluye 24 casos de guardado |
| Tests C++ | Controles y cadencia correctos |
| Batería real | Crystal y Emerald completan guardar/cerrar/continuar; NDS sigue pendiente |
| Salida de combate NDS | Huida a 1x y victoria a 5x verificadas visualmente con el intervalo corregido |
| Matriz de mantenimiento anterior | Release/Debug con Visual Studio y núcleo con Ninja/Ninja Multi-Config; no equivale a una nueva ejecución |

Las campañas de 383/126 y 397/129 pruebas describían revisiones anteriores y no
deben sumarse al resultado posterior. Las pruebas normales no incluyen sondas
ignoradas ni certifican todos los juegos.

La campaña NDS anterior a corregir la salida de combate alcanzó alrededor de
5x con PGO en escenas concretas. Tras corregir la sincronización, muestras de unos
19,5 segundos registraron **3,2938x en exterior** y **4,5009x en interior con
movimiento**, con PGO, VSync OFF y audio real a 96 kHz. No acreditan 5x sostenido
ni el rendimiento de cualquier binario posterior.

La última validación de guardado usó
`build/save-review-20260910/native/bin/Release/clothing_app.exe`,
copiado entonces a la raíz; el antiguo `build/bin/Release/clothing_app.exe`
no incorporaba esas correcciones en ese momento. Su SHA-256 registrado fue
`e85cadaafd91cda07c3f8240ae4e7f572a72c05837a008f37cb2b783e16ab682`.
Es una identificación histórica, no una afirmación sobre el archivo que exista
ahora. CMake ya contiene la copia posterior a compilación, pero ese paso automático
no se pudo volver a ejecutar en aquella validación por restricciones del entorno.

Persisten estos límites:

- SoulSilver no completa continuar desde batería: aparece «Communication error»
  con JIT activado y desactivado, pese a corregirse la escritura parcial.
- F2/F5/F9 y el diálogo visual de error requieren comprobación manual adicional;
  la ruta compartida de guardado/carga sí pasó pruebas del protocolo nativo.
- No se validaron Linux/macOS, versiones antiguas de Rust ni toda la compatibilidad
  de cartuchos. Las pruebas de WRAM NDS no demuestran fidelidad completa al hardware:
  la representación existente usa 256 KiB frente a los 32 KiB compartidos físicos.
- Un estado o batería que ya perdió datos no se reconstruye con estas correcciones.

La evidencia local, excluida de Git y disponible sólo si se conserva el build,
está en `build/maintenance-20260910/`, `build/nds-5x-20260910/` y
`build/save-review-20260910/`. Este último contiene `pytest-final.log`,
`rust-workspace-final.log` y las capturas de recuperación de partidas.
La campaña PGO corregida usó `nds-visual-fix.profdata` y registró un desajuste
de perfil de `load_state`: LLVM descartó esos conteos. No reutilice ese perfil
como garantía de una compilación nueva.
