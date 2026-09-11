# Guía de uso del emulador

## Preparación y compilación

La aplicación usa Rust, C++17 y SDL2. En Windows necesita MSVC Build Tools con
C++ y Windows SDK, Cargo en PATH, CMake 3.20+ y Python 3.10+.
La configuración probada usa Rust 1.96.1; las dependencias fijadas requieren al
menos Rust 1.85. SDL2 se mantiene en la versión 2.32.10.

```powershell
python -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements-test.txt
.\.venv\Scripts\python.exe install_sdl2.py --download-only
.\.venv\Scripts\python.exe run_build_and_test.py
```

Para compilar sin ejecutar pruebas:

```powershell
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DSDL2_DIR=sdl2/SDL2-2.32.10/cmake
cmake --build build --config Release --target clothing_app --parallel 2
```

Visual Studio selecciona la configuración al construir. Use `--config Release`
para jugar y medir rendimiento, o `--config Debug` para depurar. CMake selecciona
el perfil Rust correspondiente y copia SDL2.dll junto al ejecutable. Cada
compilación actualiza también `clothing_app.exe` y `SDL2.dll` en la raíz del
proyecto, incluida Debug; compile Release para dejar allí la versión para jugar.
Cierre la aplicación antes de reemplazar su ejecutable. La compilación incremental basta;
no es necesario borrar build o las cachés.

`install_sdl2.py` sin `--download-only` instala el SDK y compila Release.
`--build-dir` permite elegir otro directorio. Para un build solo del núcleo,
use `-DEMULATOR_BUILD_FRONTEND=OFF` o `cargo build --locked -j 2`.
Si un build existente conserva otra versión de SDL2 en caché, ejecute el
instalador completo o configure explícitamente `-DSDL2_DIR` como arriba.
En Linux/macOS instale SDL2 con el gestor del sistema; el instalador Python es
exclusivo de Windows. Esas plataformas no se han validado en este mantenimiento.

## Abrir juegos

```powershell
.\clothing_app.exe
.\clothing_app.exe --rom "ruta\juego.nds"
```

Sin `--rom` se abre el explorador, inicialmente en `roms/` al ejecutar desde la
raíz. Admite `.gb`, `.gbc`, `.gba` y `.nds`. Flechas arriba/abajo seleccionan;
Enter o Espacio abren; Backspace sube de carpeta.

## Controles predeterminados

| Botón | Tecla |
|---|---|
| Cruceta | Flechas |
| A / B | A / S |
| L / R | Q / W |
| Start / Select | Enter / C |
| X / Y de NDS | X / Y |
| Pantalla táctil NDS | Clic o arrastre con botón izquierdo sobre la pantalla inferior |

Escape abre ajustes y pausa el juego. Seleccione un botón y pulse Enter/Espacio
para reasignarlo. Escape cancela. Las teclas duplicadas o reservadas se rechazan
con un mensaje; la asignación anterior se conserva. Los ajustes también permiten
cambiar velocidad y escala. La velocidad solicitada va de 0.5x a 5x; la velocidad
alcanzada depende del juego, la escena y el equipo.

## Partidas

F2 abre el menú de estados: arriba/abajo eligen ranura, izquierda/derecha/Tab
alternan guardar/cargar, Enter/Espacio confirman. Escape o F2 cierran el menú.
Los números 0–9 eligen ranura; F5 guarda y F9 carga. Ctrl+R solicita reiniciar.

En Windows, los ajustes y estados se guardan normalmente bajo:

```text
%APPDATA%\EmulatorApp\EmulatorApp\
```

Los estados nuevos se identifican por nombre, huella del contenido de la ROM y
ranura: `<nombre>_<huella>_savestate_<ranura>.sav`. Dos ROM diferentes con el mismo
nombre tienen ranuras separadas. La carga comprueba integridad e identidad antes
de aplicar el estado. GBC/GBA usan JSON; NDS usa un contenedor binario versionado.

Se siguen buscando los archivos anteriores `<nombre>_savestate_<ranura>.sav`.
Los genéricos `savestate_<ranura>.sav` no se asignan automáticamente a un juego:
para recuperar uno, haga una copia con el nombre antiguo de la ROM correcta.
Los archivos antiguos carecen de la huella completa y no permiten comprobar esa
identidad con la misma precisión. Los estados GBC/GBA anteriores siguen
siendo legibles, pero no contienen toda la información añadida en esta revisión:
el momento exacto de sonido, vídeo o una transferencia activa no puede recuperarse
si nunca se guardó. Los estados nuevos incluyen esa información. No se reescriben
los archivos antiguos al cargarlos.

El guardado interno del juego (batería/SRAM/Flash) se guarda junto a la ROM como
`<nombre>.sav`, y es distinto de un estado rápido. Se escribe periódicamente y al
salir o cambiar de juego. Cargar un estado rápido también deja su batería pendiente
de escritura, por lo que al cerrar persistirá la partida restaurada.

Un error de escritura conserva los datos pendientes y se muestra en pantalla.
Si ocurre al cerrar o volver al explorador, el juego permanece abierto: libere
espacio o restablezca el acceso a la carpeta de la ROM y vuelva a intentarlo.
En modo sin ventana el error se informa con un código de salida distinto de cero.
El comando interactivo `FLUSH_BATTERY` permite intentar la escritura explícitamente.

La compatibilidad actual de chips de guardado es MBC3/SRAM con RTC en GBC,
Flash de 128 KiB en GBA y Flash de 512 KiB en NDS. Son los chips utilizados por
Crystal, Emerald y SoulSilver; no implica compatibilidad con todos los cartuchos,
SRAM/EEPROM de GBA u otros chips de NDS. Haga copias antes de sustituir partidas.

Validación del 10-09-2026: Crystal y Emerald completan guardar, cerrar y continuar
desde batería. En SoulSilver se corrigió la corrupción de escrituras parciales,
pero el arranque desde una partida válida aún muestra «Communication error»,
con JIT activado y desactivado. El ciclo completo de NDS sigue pendiente.
Emerald muestra un aviso de reloj interno agotado, aunque recupera la partida.
Consulte [las pruebas y sus límites](docs/SAVE_VALIDATION_2026-09-10.md).

## Ejecución sin ventana

```powershell
.\clothing_app.exe --headless --rom "ruta\juego.gba" --play --ticks 120
.\clothing_app.exe --interactive
```

`--speed`, `--frame-skip`, `--dump-video`, `--dump-audio` y `--dump-state` permiten
preparar pruebas. El audio exportado es PCM estéreo de 16 bits. Headless escribe
audio incrementalmente solo si se solicita; una exportación fallida devuelve
un código de salida distinto de cero. En el protocolo interactivo, los comandos
`DUMP_*` restringen sus destinos mediante `ALLOWED_DUMP_DIR` cuando está definida;
las opciones headless `--dump-*` escriben en la ruta indicada.

## Opciones NDS/JIT

En Windows x64, los JIT ARM9 y ARM7 están activos por defecto. Para comparar
contra el intérprete desde PowerShell:

```powershell
$env:EMU_ARM9_JIT = "0"
$env:EMU_ARM7_JIT = "0"
.\clothing_app.exe
Remove-Item Env:EMU_ARM9_JIT, Env:EMU_ARM7_JIT
```

ARM9 usa encadenamiento y despacho indirecto; ARM7 no encadena por defecto.
`EMU_ARM9_JIT_THUMB=1` y `EMU_ARM7_JIT_THUMB=1` habilitan compilación Thumb
experimental. `EMU_NDS_SLICE` cambia el intervalo de planificación y se reserva
para diagnóstico. Los detalles actuales están en `core/src/jit/mod.rs`.

`EMU_STATE_DIR` y `EMU_STATE_SLOT` seleccionan partidas para sondas de desarrollo;
no cambian por sí mismas el directorio de estados de la interfaz. Las mediciones
históricas de 5x se conservan en [su registro](docs/history/NDS_PERFORMANCE_2026-07.md).
Para una comparación nueva utilice la misma escena, configuración y binario.
