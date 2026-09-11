# Nintendo DS: validar avance rápido

Evidencia del 10 de septiembre de 2026: `build/nds-5x-20260910/`.
La campaña de rendimiento anterior evaluó `build/bin/Release/clothing_app.exe`,
Release portable con PGO, antes de corregir la corrupción al salir de combate.
Sus cifras y hashes se conservan como referencia; no certifican la corrección actual.
La compilación normal no usa PGO: véase [la receta reproducible](NDS_PGO.md).

## Métrica

Referencia: Intel Core i9-13980HX, RTX 4080 Laptop GPU y Windows.
Registrar hash del binario, compilador, energía, monitor, VSync y audio.
Comparar consecutivamente, sin compilaciones ni pruebas de CPU simultáneas.

`cpu_cycles` mide bus entregado a periféricos. La relación
**ARM9 : ARM7 : bus = 2 : 1 : 1** conserva el exceso de ejecución por CPU entre
segmentos, sin adelantar PPU, audio ni temporizadores.

```text
segundos_emulados = delta(cpu_cycles) / 33_513_982
velocidad_real    = segundos_emulados / segundos_monotónicos
frames_PPU       ≈ delta(cpu_cycles) / 560_190
```

La fase permite aproximadamente un frame de diferencia PPU/bus.
FPS de presentación o ticks por velocidad solicitada no prueban avance real.

## Escenas aisladas

Copias de `Pokemon - SoulSilver Version (USA).nds` y estos estados:

| Slot | Escena | Input del smoke |
| --- | --- | --- |
| `0` | Exterior | Abajo 40 ticks; derecha tiene obstáculos. |
| `room` | Interior | Derecha 40 ticks. |
| `battle` | Introducción | Esperar 120; ocho pulsos A de 2 ticks/14 sueltos; esperar 60. |

Copiar ROM, batería y estados por ejecución; conservar hashes antes/después.
Estados: `<nombre-ROM>_savestate_<slot>.sav`. No usar originales ni redistribuir datos.
Las sondas desactivan su destino de batería; la GUI requiere aislamiento porque guarda.

Ejemplos desde la raíz, con copia de trabajo ya preparada:

```powershell
$lab = Join-Path (Get-Location).Path 'build/nds-5x-20260910'
$run = "$lab/repro"
$rom = "$run/Pokemon - SoulSilver Version (USA).nds"
$exe = 'C:/ruta/al/ejecutable-definitivo/clothing_app.exe'
```

## Sondas del núcleo

`wall_clock_speed_ceiling_probe` excluye presentación/regulación GUI. Recarga cada
muestra y calienta aproximadamente un segundo emulado a 1x.

| Variable | Valores |
| --- | --- |
| `EMU_BENCH_ROM` | NDS; ausente mantiene barrido de tres consolas. |
| `EMU_STATE_DIR`, `EMU_STATE_SLOT` | Escena exacta; slot predeterminado `0`. Sin directorio mide arranque. |
| `EMU_BENCH_WINDOW_MS` | 1–300000; predeterminado 1500. |
| `EMU_BENCH_REPEATS` | 1–20; predeterminado 3. |
| `EMU_BENCH_SPEEDS` | Lista admitida; predeterminado `1,4,5`. |
| `EMU_BENCH_FRAME_SKIP` | 0–9; ausente conserva el estado. |
| `EMU_BENCH_AUDIO_HZ` | 8000–192000; NDS predeterminado 48000 Hz. |

Audio/frame skip se aplican y registran antes de calentar. Repetir con `room` y
`battle`, manteniendo build y flags:

```powershell
cmake -E env "EMU_BENCH_ROM=$rom" "EMU_STATE_DIR=$run" EMU_STATE_SLOT=0 'EMU_BENCH_SPEEDS=1,5' EMU_BENCH_WINDOW_MS=8000 EMU_BENCH_REPEATS=3 EMU_BENCH_FRAME_SKIP=0 EMU_BENCH_AUDIO_HZ=48000 cargo test --locked --release -p emulator_core wall_clock_speed_ceiling_probe -- --ignored --nocapture --test-threads=1 2>&1 | Tee-Object "$run/core.log"
```

Filtro `nds_interpreter_cost_probe`: CPU/3D/2D/resto y swaps iniciales/finales/por frame.
Comparte ROM/estado/audio/frame skip; fija 1x/5x, calentamiento de 30 ticks y 120 medidos.
No consume ventana/repeticiones/speeds. Instrumentación añade coste; `SKIP` no aprueba.
La sonda de techo falla si la escena explícita no carga.

## GUI y agregación

```powershell
cmake -E env "ALLOWED_DUMP_DIR=$run" EMU_SPEED_STATS=1 EMU_AUDIO_STATS=1 EMU_BENCH_SECONDS=300 "$exe" --rom "$rom" --load-state 0 --speed 5 --frame-skip 0 --play 2> "$run/gui.log"
```

`--load-state` exige ROM y `LOAD_STATE_OK`; escena inválida termina antes de abrir
dispositivos, sin sustituirla por arranque. `ALLOWED_DUMP_DIR` selecciona estados/salidas;
speed/frame skip explícitos prevalecen sobre ajustes restaurados.

`EMU_SPEED_STATS` se activa por presencia, incluso valor `0`. Ventanas ≈0,5 s:
`requested`, `achieved`, `emulated_s`, `wall_s`, `core_s`, `ticks_count`, `loop_fps`,
`vsync`. Pausas/restauraciones/cambios de velocidad reinician ventanas; el fragmento
final puede omitirse. `loop_fps` cuenta iteraciones, incluidas las que no producen una
imagen nueva; no mide velocidad emulada. VSync se consulta al renderer SDL real.
En DS a partir de 2x se añaden `composed_fps` y `max_video_gap_ms`: frecuencia de
composición e intervalo máximo entre composiciones. No cuentan imágenes de contenido
distinto ni acreditan su presentación física en el monitor.

`EMU_BENCH_SECONDS`: duración positiva finita de GUI, incluidas pausas; no limita
sondas Rust ni protocolo interactivo.
Repetir tres escenas y 1x→5x→1x: pantallas, controles, guardado/carga y audio real.
Audio ficticio, afinidad o menor frecuencia son diagnósticos separados.

Logs por stderr, no CSV nativo. CSV derivado conserva fuente, hash, escena y ajustes.
Agregar `sum(emulated_s)/sum(wall_s)`, sin promediar multiplicadores ni mezclar builds.
Percentiles del núcleo no miden latencia GUI; PCM no vacío no acredita audio audible.

## Compatibilidad y evidencia

La GUI conserva el plazo acumulado de emulación. A partir de 2x puede omitir la
composición de ticks intermedios atrasados; CPU, audio y periféricos siguen avanzando.
Comprueba entrada tras cada tick y limita la recuperación a ocho ticks/ocho periodos.
El plazo de vídeo se mantiene entre lotes interrumpidos por entrada. Parte de 50 ms
y admite un periodo adicional (≈16,7 ms) sólo con retraso y si un tick sin dibujo
medido cabe en un periodo. Cuando ni omitiendo vídeo se recupera tiempo, conserva
el plazo más corto para favorecer la fluidez. Son plazos orientativos: el coste
del siguiente tick, la presentación y VSync pueden prolongarlos.

En ajustes, **FRAME SKIP** aparece después de **SPEED** y admite valores de 0 a 9.
El valor 0 evita la omisión manual de frames; la omisión adaptativa de composiciones
atrasadas sigue activa. Son controles independientes.

Swaps diferidos retienen geometría, viewport, limpieza, profundidad y VRAM inmutables.
Separan pendiente/publicado en VBlank; resuelven sin registros posteriores ni asumir
un swap/frame. La ventana visible es completa.

Snapshot v3 conserva créditos CPU, imágenes 3D frontal/posterior, publicación y
geometría parcial; materializa raster al guardar. Acepta v2 con créditos cero y sin
inventar trabajo 3D ausente. Conservar v2: lectores antiguos no garantizan v3.
Comprobar relojes, IPC, snapshots y paridad raster diferido/inmediato.
Regenerar capturas diagnósticas .snap antiguas sin cabecera; los estados .sav v2 sí son compatibles.

`validate_soulsilver.py <exe>` usa harness/Pillow y copias `smoke-*`: 24 ticks por fase
1x/5x/1x, ciclos/presentación, guardado/restauración, input a 1x contra control de igual
duración y PCM acotado. Exporta RAW RGB888 256×384, PNG y JSON con hashes.
`final-scene-smoke.log` y `smoke-7s8ux3kn/report.json` verifican tres escenas mediante
ventanas breves con coste IPC; no certifican GUI ni input sostenido a 5x. Estas
pruebas de la campaña anterior no incluyeron la salida del combate.

PGO de la campaña anterior: perfil `nds-final.profdata`, 314432 bytes, SHA-256
`3f50311ff8342cfb1ca67860e26d699b8217c8247cf7f99ebac6430e12af0cc8`.
Ejecutable de 1109504 bytes, SHA-256
`8ba7c8c11cb05ac4cee77019e083a5bd38219e8add98ffb6bd6914280969981b`.
En esa compilación, el núcleo no tenía perfiles faltantes ni desajustes de hash.
Los avisos de perfil ausente del script de compilación no afectaban al núcleo entrenado.

## Resultados de la campaña anterior

Estos resultados pertenecen al binario anterior, que aún podía corromper los
personajes al salir de combate. Deben repetirse con la corrección actual antes de
atribuirle el mismo rendimiento.

Windows, i9-13980HX, renderer SDL Direct3D, audio real estéreo a 96000 Hz,
VSync OFF, velocidad solicitada 5x y frame skip manual 0. Cada ejecución usa
su propia copia de ROM/estado. No se compila ni ejecuta otra suite durante la medida.
Las cifras son `sum(emulated_s)/sum(wall_s)`; redondear a 5,00 no significa ausencia
de fluctuaciones instantáneas ni 300 imágenes distintas por segundo.

| Escena SoulSilver | Sesión larga | Tres repeticiones de 60 s |
| --- | --- | --- |
| Exterior | **4,9751x**, 299,74 s medidos | **4,9835 / 4,9893 / 5,0005x** |
| Interior | **5,0005x**, 299,89 s medidos | **5,0030 / 5,0018 / 5,0016x** |
| Introducción/menú de combate | **5,0005x**, 166,45 s medidos, parcial | No ejecutadas |

El usuario pidió terminar las pruebas al ver funcionar la aplicación. La campaña
se detuvo durante combate: el código de salida -1 del último proceso es consecuencia
de esa interrupción, no un fallo espontáneo. El tramo final coincidió con otra
instancia abierta. No se completaron los cinco minutos ni las repeticiones de combate,
ni la comprobación GUI adicional a 1x que estaba prevista.

En exterior hubo una ventana breve de 2,9933x; el promedio largo alcanza el 99,50 %
del objetivo. Ese binario alcanzó **aproximadamente 5x real en estas escenas**,
con variación temporal, sin garantizar 5x constante en cualquier juego o equipo.
La presentación adaptativa reduce trabajo visual cuando falta margen de CPU.
Desactivar VSync permite evitar su espera, con el posible coste visual de tearing.

Evidencia: `gui-final/results.json`, `gui-final/battle-partial.json` y sus `stderr.log`.
La comparación breve previa dio 4,1036x sin PGO y 4,9466x con PGO/VSync ON;
son ejecuciones diagnósticas distintas, no una certificación de la configuración final.
Las dos muestras de memoria privada del exterior fueron 205,00 y 210,35 MB;
el pico de working set fue 404,46 MB. No constituyen por sí solas una prueba de fugas.

La revisión visual anterior de 12 capturas no detectó artefactos en las situaciones
capturadas, pero no incluyó salir del combate y no detectó esa regresión. Las partidas
originales 0/1 conservaron sus SHA-256: `original-states-final.json`. Se completaron
las pruebas funcionales previstas a 1x/5x/1x, con esa misma limitación de cobertura.

## Corrección de la salida de combate

La conversión del intervalo a ciclos de bus había mantenido el número 4096, pasando
de un máximo de 4096 ciclos nativos ARM9 a 8192 entre actualizaciones de periféricos.
Ese intervalo podía saltarse líneas completas de 2130 ciclos de bus. El valor
predeterminado actual es **2048 ciclos de bus**, que restaura el máximo anterior de
4096 ciclos ARM9 y conserva la relación de relojes **2 : 1 : 1**.

La corrupción del personaje y del Pokémon acompañante se reprodujo al huir tanto
a 1x como a 5x. Repetir la salida con el intervalo de 2048 evitó el patrón observado:
`battle-repro/slice2048-flee1/2.png` y `battle-repro/slice2048-flee5/2.png`.
El binario corregido, sin variables de intervalo, también pasa la huida a 1x
(`battle-repro/verified-flee1/2.png`) y la victoria completa a 5x
(`battle-repro/verified-win5/14.png`). Las capturas muestran al personaje y al
acompañante intactos al volver al mapa. La nueva regresión de VCOUNT falla con
4096 bus y pasa con 2048: `vcount-old-window.log` y `fix-rust-tests.log`.

Un estado guardado que ya contiene la corrupción no se repara al volver a 1x ni
al cambiar el intervalo. Cargar un estado anterior a la pelea y repetirla con la
corrección permite comprobar la transición desde datos intactos.

## Medición breve de la versión corregida

PGO portable, SDL Direct3D, VSync OFF, audio real 96 kHz, velocidad solicitada 5x
y frame skip manual 0. Dos ejecuciones de 20 segundos, con copias de las escenas;
en interior se inyectó movimiento alternado izquierda/derecha en cada tick.

| Escena | Tiempo registrado | Velocidad real media | Composiciones/s | Mayor intervalo |
| --- | --- | --- | --- | --- |
| Exterior | 19,49 s | 3,2938x | 24,27 | 72,15 ms |
| Interior con movimiento | 19,48 s | 4,5009x | 22,18 | 78,79 ms |

Evidencia: `fix-gui-release/results.json` y logs por escena. Son muestras breves
de diagnóstico, no una nueva campaña prolongada. Hubo ventanas cercanas a 5x en
interior, pero **esta revisión no acredita 5x sostenido**. La corrección de
sincronización se conserva incluso cuando el equipo no entrega la velocidad
solicitada; no se sustituye velocidad real por FPS o multiplicadores declarados.

Los costes observados de ticks sin dibujo también llegaron a superar los
16,7 ms disponibles. Ampliar siempre la espera visual no resuelve ese límite:
por eso sólo se concede margen adicional cuando la muestra de coste permite
recuperar tiempo. El plazo inicial queda fijado incluso antes de la primera
imagen, y los eventos posteriores no fuerzan dibujos extra en cada segundo tick.

Binario entregado: SHA-256
`50e2b43c8833b6f70e8c43968a57b457b7ccb04be928ec8175db98f183667ba3`.
Perfil: `nds-visual-fix.profdata`; procedencia y aviso de `load_state` en
[NDS_PGO.md](NDS_PGO.md). Validación: `fix-release-native-tests.log`,
`fix-final-pacing-tests.log`, `fix-rust-tests.log` y `fix-original-states.json`.

## Abrir la versión actualizada

La versión corregida se entrega en `build/bin/Release/clothing_app.exe`.
Mantener `SDL2.dll` junto al ejecutable; no hace falta recompilar para probarla.
Cerrar la instancia anterior, abrir el ejecutable desde la raíz del proyecto,
cargar un estado anterior a la pelea y usar Escape → ajustes:
**SPEED 5x, FRAME SKIP 0, VSYNC OFF**.
Para registrar velocidad real, definir `EMU_SPEED_STATS=1` antes de iniciar la app.

Una compilación normal posterior vuelve a construir sin PGO. Para conservar esta
optimización seguir [NDS_PGO.md](NDS_PGO.md), regenerando perfiles si cambia el código.
