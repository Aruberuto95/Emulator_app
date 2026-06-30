# Guía de uso del emulador

Emulador Game Boy Color / Game Boy Advance (núcleo Rust + frontend C++/SDL2).

---

## 1. Compilar

Requiere Rust (cargo), CMake, MSVC Build Tools y el SDK de SDL2 en `sdl2/`. En este equipo el toolchain ya está instalado.

```powershell
# Opción A — Debug + correr la suite de tests (recomendado al desarrollar)
$env:CARGO_BUILD_JOBS = "2"
.\.venv\Scripts\python.exe run_build_and_test.py
# Ejecutable -> build\bin\Debug\clothing_app.exe

# Opción B — Build Release (más rápido para jugar) + copia de SDL2.dll
.\.venv\Scripts\python.exe install_sdl2.py
# Ejecutable -> build\bin\Release\clothing_app.exe
```

> `CARGO_BUILD_JOBS=2` limita los hilos de compilación para no saturar la RAM.

El `.exe` necesita `SDL2.dll` en su misma carpeta (los scripts ya la copian).

---

## 2. Ejecutar

Correr **desde la raíz del proyecto** (para que se encuentre la carpeta `roms/`).

### Sin ROM (explorador de archivos)

```powershell
.\build\bin\Debug\clothing_app.exe
```

Arranca directo en el explorador. Permite navegar carpetas y elegir el juego:

| Tecla            | Acción                                  |
|------------------|-----------------------------------------|
| ↑ / ↓            | Mover selección                         |
| Enter / Espacio  | Abrir carpeta `[DIR]` o cargar ROM      |
| Backspace        | Subir a la carpeta superior (`[..]`)    |

Muestra solo archivos `.gb` / `.gbc` / `.gba`. La carpeta inicial es `roms/`; desde ahí se puede navegar a cualquier otra ubicación.

### Con ROM (carga directa)

```powershell
.\build\bin\Debug\clothing_app.exe --rom "roms\Pokemon - Crystal Version (UE) (V1.1) [C][!].gbc"
```

### Otros argumentos

| Argumento              | Descripción                                  |
|------------------------|----------------------------------------------|
| `--rom <ruta>`         | Cargar una ROM al iniciar                    |
| `--speed <x>`          | Velocidad (`0.5`, `1.0`, `2.0`, `4.0`)        |
| `--frame-skip <n>`     | Saltar `n` frames de render (con velocidad >1x) |
| `--interactive`        | Modo consola por stdin (sin ventana)         |
| `--headless --ticks N` | Correr N frames sin render (para pruebas)    |

---

## 3. Controles

### Mapeo por defecto

| Botón emulador | Tecla         |
|----------------|---------------|
| Cruceta        | Flechas ← ↑ → ↓ |
| A              | `a`           |
| B              | `s`           |
| L              | `q`           |
| R              | `w`           |
| Select         | `z`           |
| Start          | `x`           |

### Reconfigurar las teclas

1. Durante el juego, pulsar **Esc** → abre el menú de ajustes (pausa el juego).
2. ↑ / ↓ para elegir el botón a remapear.
3. Enter / Espacio → muestra `PRESS ANY KEY...`.
4. Pulsar la tecla nueva (Esc cancela el remapeo sin cambiar nada).
5. **Esc** para salir del menú y reanudar.

Los cambios se guardan al instante y persisten entre sesiones en:

```
%APPDATA%\EmulatorApp\EmulatorApp\input_mappings.json
```

> Si ese archivo no existe, se usan los valores por defecto de la tabla anterior.

---

## 4. Guardado de partidas

Hay **dos tipos** de guardado:

### a) Save states (10 ranuras por juego)

Capturas completas del estado del emulador. Cada juego tiene sus propias 10 ranuras (`0`–`9`), independientes de otros juegos, y persisten entre sesiones.

**Menú visual (recomendado):** pulsar **F2** durante el juego.

| Tecla            | Acción                                   |
|------------------|------------------------------------------|
| ↑ / ↓            | Elegir ranura (0–9)                      |
| ← / → / Tab      | Cambiar entre modo **SAVE** y **LOAD**   |
| Enter / Espacio  | Confirmar (guardar o cargar la ranura)   |
| Esc / F2         | Cerrar el menú                           |

Cada ranura muestra `[EMPTY]` o la fecha del guardado.

**Atajos rápidos (sin menú):**

| Tecla   | Acción                                  |
|---------|-----------------------------------------|
| `0`–`9` | Seleccionar la ranura activa            |
| `F5`    | Guardar en la ranura activa             |
| `F9`    | Cargar la ranura activa                 |

Los archivos se guardan en:

```
%APPDATA%\EmulatorApp\EmulatorApp\<nombre_rom>_savestate_<ranura>.sav
```

Como llevan el nombre de la ROM, cargar otro juego **no** sobrescribe las partidas del anterior.

### b) Save de batería (SRAM `.sav`)

Es el guardado *interno del propio juego* (en Pokémon: "GUARDAR" en el menú). Se escribe automáticamente junto a la ROM:

```
roms\<nombre_rom>.sav
```

No requiere acción manual; se sincroniza solo mientras juegas.

---

## 5. Resumen de teclas por pantalla

| Pantalla        | Teclas                                                              |
|-----------------|--------------------------------------------------------------------|
| Explorador      | ↑↓ mover · Enter abrir/cargar · Backspace subir                    |
| Juego           | controles mapeados · Esc ajustes · F2 menú guardado · F5/F9 save/load rápido · 0–9 ranura |
| Menú ajustes    | ↑↓ navegar · Enter remapear · Esc salir                            |
| Menú guardado   | ↑↓ ranura · ←→ SAVE/LOAD · Enter confirmar · Esc cerrar            |
