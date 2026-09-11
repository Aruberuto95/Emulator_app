> Registro histórico anterior al mantenimiento de septiembre de 2026. Sus cifras y conclusiones no validan la versión actual. Consulte [la guía vigente](../../GUIA_DE_USO.md) y [las validaciones](../../TEST_READY.md).

# Guía de uso — build y ejecución del emulador

## Requisitos (ya instalados en esta máquina, fuera del PATH)

- Rust/cargo: `C:\Users\alber\.cargo\bin`
- CMake: `C:\Program Files\CMake\bin`
- MSVC BuildTools 2022
- SDL2 en la carpeta `sdl2/` del repo (sin ella el exe no se genera y no da error visible)

## Build desde cero (limpio)

En la terminal PowerShell del IDE, desde la raíz del repo:

```powershell
# 1. Borrar el build anterior
Remove-Item -Recurse -Force build

# 2. Toolchain al PATH y límite de RAM para cargo
$env:PATH = "C:\Users\alber\.cargo\bin;C:\Program Files\CMake\bin;$env:PATH"
$env:CARGO_BUILD_JOBS = "2"

# 3. Configurar y compilar en Release
cmake -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build --config Release -j 2
```

El ejecutable queda en:

```
build\bin\Release\clothing_app.exe
```

Alternativa que hace todo lo anterior y además corre los tests:

```powershell
.venv\Scripts\python.exe run_build_and_test.py
```

## Cómo actualizar el emulador a la build 5x (NDS)

Toda la configuración del 5x — recompilador ARM9, recompilador ARM7 y el
slice de 4096 — **viene por defecto en el código fuente de esta rama**
(`feat/nintendo-ds-2`). No hay que tocar ninguna variable ni ningún menú:
solo recompilar el exe para que incorpore el core nuevo.

```powershell
# 1. Cerrar el emulador si está abierto (trampa 3: una instancia abierta
#    nunca toma el binario nuevo)

# 2. Toolchain al PATH y límite de RAM para cargo
$env:PATH = "C:\Users\alber\.cargo\bin;C:\Program Files\CMake\bin;$env:PATH"
$env:CARGO_BUILD_JOBS = "2"

# 3. Recompilar en Release (incremental; no hace falta borrar build)
cmake --build build --config Release -j 2

# 4. Lanzar
.\build\bin\Release\clothing_app.exe
```

Si `build\` no existe todavía, usar antes el paso de configuración de la
sección anterior (`cmake -B build -DCMAKE_BUILD_TYPE=Release`). Y siempre
`--config Release` — la trampa 1 sigue vigente: un exe Debug es lento y ya
produjo diagnósticos falsos.

Para comprobar que quedó bien: cargar SoulSilver, entrar al overworld y
poner el fast-forward a 5x. Medido en el core (headless, savestate del
overworld): **5.14-5.22x logrados con petición de 5x** (el intérprete puro
da ~3.5x en la misma escena). Recordá la trampa 4: el velocímetro de la app
marca menos que el probe headless porque incluye render, audio y present —
la señal de que la build es la nueva es el salto grande respecto a la
anterior, no el número absoluto.

Verificación opcional sin abrir la app (desde la raíz del repo):

```powershell
$env:EMU_STATE_DIR = "C:/Users/alber/AppData/Roaming/EmulatorApp/EmulatorApp"
$env:EMU_STATE_SLOT = "0"
C:\Users\alber\.cargo\bin\cargo test --release -p emulator_core --lib wall_clock_speed_ceiling_probe -- --ignored --nocapture
```

La fila `NDS ... requested= 5.0x` debe marcar `achieved` ≈ 5.1-5.4x.

Qué contiene la build 5x (todo por defecto, medido por separado):

| Pieza | Aporte medido @5x |
|---|---|
| Recompilador ARM9 (ya venía) | 3.0x → ~3.9x |
| Recompilador ARM7 (`EMU_ARM7_JIT`) | → ~3.95x |
| Multiplicaciones + LDR/STR con offset de registro | → ~4.05x |
| CP15 no-op + MRS SPSR + CLZ | → ~4.09x |
| Slice 256 → 4096 (`EMU_NDS_SLICE`) | → **~5.2x** |

## El JIT (recompiladores ARM9 y ARM7 de NDS)

Los DOS recompiladores **vienen activados por defecto** — no hay que hacer
nada, basta lanzar el exe. El del ARM9 mejora el fast-forward de NDS ~25-28%
y el del ARM7 añade otro ~5% (medidos en el overworld de SoulSilver, con
identidad bit a bit verificada frente al intérprete). Solo afectan a NDS;
GBC/GBA no los usan.

Para desactivar uno u otro (comparar contra el intérprete puro), en la misma
terminal antes de lanzar:

```powershell
$env:EMU_ARM9_JIT = "0"   # apaga el del ARM9
$env:EMU_ARM7_JIT = "0"   # apaga el del ARM7
.\build\bin\Release\clothing_app.exe
```

Variables opcionales del JIT (para experimentar; los valores por defecto ya
son los medidos como mejores):

| Variable | Por defecto | Qué hace |
|---|---|---|
| `EMU_ARM9_JIT` | `1` | Recompilador ARM9 encendido |
| `EMU_ARM7_JIT` | `1` | Recompilador ARM7 encendido (modo exacto por slice, sin encadenado) |
| `EMU_ARM9_JIT_LINK` | `1` | Encadenado de bloques ARM9 (successor linking) |
| `EMU_ARM9_JIT_DISPATCH` | `1` | Despacho indirecto ARM9 (BX/retornos) |
| `EMU_ARM9_JIT_THUMB` | `0` | Compilar también Thumb (medido: paridad, no mejora) |
| `EMU_ARM9_JIT_MIN` | `1` con link | Mínimo de instrucciones por bloque |
| `EMU_ARM7_JIT_LINK` | `0` | Encadenado ARM7 (apagado: rompería el modo exacto por slice) |

El ARM7 usa el mismo juego de sub-variables con el prefijo `EMU_ARM7_JIT_*`.

Con los dos recompiladores y el nuevo tamaño de slice (`EMU_NDS_SLICE`, por
defecto 4096 — medido: es lo que llevó el fast-forward de NDS de ~4.1x a
**~5.2x** en SoulSilver), el objetivo de 5x está cumplido. Bajar el slice
solo tiene sentido para depurar (`EMU_NDS_SLICE=256` restaura el interleave
antiguo).

## Trampas conocidas (todas quemaron tiempo real)

1. **`--config Release` es obligatorio.** El generador de Visual Studio es
   multi-config: sin `--config Release` compila **Debug**, que es demasiado
   lento para fast-forward y ya causó dos diagnósticos falsos de "audio
   robótico / gráficos rotos" que eran solo un exe viejo de Debug.
2. **Borrá `build\bin\Debug\` si aparece.** Un exe de Debug rezagado se lanza
   por error con facilidad.
3. **Cerrá el emulador antes de recompilar.** Una instancia abierta nunca toma
   el binario nuevo.
4. **Los números del velocímetro de la app no son los del probe.** Las
   mediciones del core (headless) no incluyen render, audio ni present; el
   "real" en pantalla siempre da menos. La comparación válida es JIT
   encendido vs apagado en la misma escena.

## Ejecución headless (para pruebas)

```powershell
.\build\bin\Release\clothing_app.exe --interactive
```

con stdin redirigido desde un archivo **sin BOM** (usar
`[IO.File]::WriteAllText` + `Start-Process -RedirectStandardInput`; el pipe
directo de PowerShell añade un BOM que rompe el primer comando). Ejecutar
desde la raíz del repo para que `roms/` resuelva.

La configuración y los savestates persisten en
`%APPDATA%\EmulatorApp\EmulatorApp\` (se puede redirigir con
`ALLOWED_DUMP_DIR`, y el estado con `EMU_STATE_DIR` / `EMU_STATE_SLOT`).
