# PGO portable para Nintendo DS en Windows

PGO es opt-in: la compilación normal del repositorio **no lo activa por defecto**.
Receta para PowerShell, Visual Studio 2022 x64 y el mismo checkout sin cambios entre
generación y uso. Campaña inicial: rustc 1.96.1, LLVM 22; registrar `rustc -Vv`.
Ambos builds conservan el target portable `x86_64-pc-windows-msvc`, sin `target-cpu=native`.
El flujo instrumentar/entrenar/fusionar/recompilar sigue la [guía oficial de Rust](https://doc.rust-lang.org/rustc/profile-guided-optimization.html).

## Preparar y compilar el instrumentado

Ejecutar desde la raíz en una consola dedicada. Usar rutas PGO sin espacios.
`$fixtures` debe contener **copias** de las ROM DS/GBA/GBC, baterías y estados DS
`0`, `room`, `battle`; nombres y aislamiento en [NDS_FAST_FORWARD.md](NDS_FAST_FORWARD.md).

```powershell
$repo = (Get-Location).Path
$pgo = Join-Path $repo ('build/pgo-' + [guid]::NewGuid().ToString('N'))
Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue # Prevalece sobre RUSTFLAGS.
$fixtures = 'C:/ruta/copias-pgo'
$nds = Join-Path $fixtures 'Pokemon - SoulSilver Version (USA).nds'
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
```

Comprobar éxito de cada comando. El enlace C++ de la biblioteca estática Rust necesita
`libprofiler_builtins-*.rlib` del **mismo toolchain** en `CMAKE_EXE_LINKER_FLAGS` durante
instrumentación; no usar `/WHOLEARCHIVE`. No es necesario en el build final.

## Entrenar sólo con ejecuciones completas

Crear un directorio raw nuevo **después de compilar**: excluye perfiles de build scripts.
`%m-%p` separa módulos/procesos. No mezclar raw de fuentes o compiladores distintos.

```powershell
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
Remove-Item Env:EMU_BENCH_SECONDS
```

Exigir `LOAD_STATE_OK`, al menos una copia GBA y una GBC, y raw no vacíos.
Entrenar también la GUI a 5x ejercita recuperación con ticks sin vídeo; headless no.
Dejar cerrar normalmente para escribir perfiles. El instrumentado no mide rendimiento.

## Fusionar y compilar el candidato

```powershell
$profile = Join-Path $pgo 'nds.profdata'
& $profdataTool merge -o $profile $raw
if ($LASTEXITCODE -ne 0) { throw 'Falló llvm-profdata merge' }
Remove-Item Env:LLVM_PROFILE_FILE
$env:RUSTFLAGS = "-Cprofile-use=$profile -C llvm-args=-pgo-warn-missing-function"
cmake -S . -B "$pgo/use" -G 'Visual Studio 17 2022' -A x64 "-DRUST_TARGET_DIR=$pgo/use-cargo"
cmake --build "$pgo/use" --config Release --target clothing_app --parallel 2
Remove-Item Env:RUSTFLAGS
```

Candidato: `$pgo/use/bin/Release/clothing_app.exe`. Conservar perfil, fuente, flags
y hashes. El perfil de la campaña anterior es `build/nds-5x-20260910/nds-final.profdata`.
También puede aplicarse al build raíz mediante `RUSTFLAGS=-Cprofile-use=<ruta-absoluta>`
al compilar; registrar esa procedencia y quitar la variable para volver al build normal.

Comparación preliminar anterior a corregir la salida del combate, GUI de 30 s:
Release adaptativo **4.1036x**; PGO **4.9466x**
con VSync y **5.0025x** sin VSync. No certifica duración prolongada ni garantiza 5x en
otros juegos/equipos. Validar el candidato con las escenas, audio, controles, snapshots
y agregación `sum(emulated_s)/sum(wall_s)` de [NDS_FAST_FORWARD.md](NDS_FAST_FORWARD.md).

La corrección usa `nds-visual-fix.profdata`, SHA-256
`52d45a78b082d737d811ae13916305989afd08d12b23a7551e7dbf881b4265bd`.
Se entrenó con 300 ticks por escena DS a 1x/5x, GBA/GBC a 1x, una salida de
combate a 5x y 15 segundos de GUI. Registros `fix-pgo-generate.log`,
`fix-train-*` y `fix-final-build.log`. El cambio concurrente del lector de
guardados produjo un aviso de hash distinto sólo para `load_state`: LLVM
descartó esos 188 conteos del perfil. No hubo aviso de desajuste para el bucle
de emulación. La suite nativa y la carga de las escenas validaron el binario
resultante; esta excepción queda registrada, sin mezclarla con el perfil anterior.
