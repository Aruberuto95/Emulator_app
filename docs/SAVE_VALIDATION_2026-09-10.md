# Guardado de partidas: correcciones y validación

Fecha: 10 de septiembre de 2026. Revisión solicitada con Ponytail; implementación
autorizada posteriormente mediante «procede».

## Resultado

| Consola | Estado rápido y persistencia | Guardado del juego y arranque desde batería |
| --- | --- | --- |
| GBC | Pruebas nativas correctas | Crystal: correcto; Ash, mismo lugar y tiempo 1:03 |
| GBA | Pruebas nativas correctas | Emerald: correcto; Aru, laboratorio de Littleroot, 0:06 |
| NDS | Pruebas nativas correctas; escritura parcial corregida | SoulSilver: fichero íntegro, pero aparece «Communication error» antes de poder continuar |

**No se certifica todavía el ciclo completo de NDS ni todos los cartuchos de cada
consola.** El backend actual cubre MBC3/SRAM/RTC, GBA Flash128 y NDS Flash512K.

## Cambios

- Restaurar un estado deja pendiente la escritura de la batería restaurada en
  GBC, GBA y NDS. Antes, la siguiente apertura podía recuperar la batería anterior.
- Los errores de batería llegan al núcleo, FFI y frontend. Una escritura fallida
  conserva los datos pendientes; el cierre y la vuelta al explorador permiten
  reintentar. El modo sin ventana informa el fallo y devuelve salida no nula.
- Los estados nuevos llevan una huella del contenido completo de la ROM, además
  del nombre y la ranura. La carga rechaza una ROM diferente incluso si tiene el
  mismo nombre, título, identificador y tamaño.
- JSON real mediante `serde_json`, integridad en estados nuevos y validación de
  regiones antes de modificar la máquina. Se eliminó el analizador manual.
- La lectura GBC reutiliza la rutina acotada de SRAM/RTC y el avance del reloj en
  tiempo constante. Las baterías GBC truncadas producen un error explícito.
- NDS Page Write conserva los bytes no direccionados de la misma página. Antes
  borraba esos bytes y una escritura posterior del cierre destruía parte de la
  partida. La prueba anterior exigía ese comportamiento incorrecto; ahora
  comprueba conservación de contenido y una segunda escritura de cierre.

La semántica de Page Write se contrastó con la ficha del fabricante del
[M45PE40](https://www.infinite-electronics.kr/datasheet/9d-M45PE40-VMW6TG-TR.pdf),
sección 6.7: los bytes no escritos permanecen intactos.

Se mantiene el campo antiguo de seguimiento de borrado como dato reservado del
snapshot para no cambiar su disposición binaria. NDS acepta contenedores v2/v3
y escribe v4 con la huella de ROM. Los estados JSON antiguos asociados por nombre
siguen accesibles. Los archivos genéricos sin ROM ya no se eligen automáticamente.

## Evidencia

- Rust, workspace completo en Release: **398 aprobadas, 27 omitidas** (sondas
  manuales o dependientes de ROM). Sin fallos.
- Python contra el ejecutable nativo actualizado: **159 aprobadas, 1 omitida**.
  Incluye 24 casos nuevos de persistencia, corrupción, identidad, compatibilidad,
  errores de lectura/escritura y recuperación tras un fallo.
- Compilación C++/Rust Release correcta. `git diff --check` sin errores.
- Crystal: guardado desde su menú, cierre del proceso, apertura sin snapshot,
  selección de Continue y regreso a la misma ubicación.
- Emerald: mismo ciclo, con el jugador Aru y regreso al laboratorio. El aviso de
  reloj interno agotado permanece; no impidió recuperar la partida.
- SoulSilver: el fallo original producía el aviso de partida corrupta al arrancar.
  Tras corregir Page Write, los bloques en `0x40000` y `0x4F700` tienen CRC16
  calculados iguales a los almacenados (`F120` y `D270`, respectivamente, en esta
  partida de prueba). La comprobación utilizó la disposición de bloques y cierres
  documentada por [pret/pokeheartgold](https://github.com/pret/pokeheartgold/blob/master/src/save.c).

Todas las operaciones de juego se hicieron sobre copias bajo
`build/save-review-20260910/games`. Las huellas SHA-256 de las ocho ROM y partidas
originales usadas como referencia permanecieron idénticas tras las pruebas.

## Pendientes y límites

1. **Arranque NDS desde batería:** SoulSilver muestra «Communication error» con
   ambos JIT activos y con ambos desactivados. La corrupción del fichero está
   corregida; la causa del error posterior de comunicaciones no está resuelta.
   Se observó el estado de error 5 del gestor de comunicaciones del juego. No se
   han alterado la ROM ni el guardado para saltarse esa comprobación.
2. **Validación visual F2/F5/F9:** la ventana de Crystal cargó correctamente, pero
   los eventos del teclado automatizado no produjeron una respuesta verificable.
   La ruta de guardado/carga compartida sí está probada mediante el protocolo
   nativo. Los atajos y el diálogo visual de error necesitan comprobación manual.
3. **Compatibilidad antigua:** los estados anteriores no contienen una huella
   completa de ROM ni todos los datos añadidos posteriormente; esas garantías no
   pueden recuperarse de información que nunca fue guardada.
4. **Baterías ya corruptas:** esta corrección no reconstruye bytes perdidos. Una
   copia de seguridad o un estado con la partida en memoria puede permitir volver
   a guardar desde el juego con la versión corregida.

## Ejecutable y reproducción

El ejecutable probado se generó en un directorio separado:

```text
build/save-review-20260910/native/bin/Release/clothing_app.exe
```

El ejecutable principal `build/bin/Release/clothing_app.exe` seguía siendo antiguo
al comprobarlo y falló los 24 casos nuevos. No usarlo para evaluar estas correcciones.

Actualización posterior solicitada: la misma versión validada está ahora en
`clothing_app.exe`, en la raíz del proyecto, junto a `SDL2.dll`. Se comprobó la
igualdad SHA-256 de ambos archivos y el arranque `--headless --ticks 1` con salida 0.
SHA-256 del ejecutable:
`e85cadaafd91cda07c3f8240ae4e7f572a72c05837a008f37cb2b783e16ab682`.
Las fuentes de la aplicación no cambiaron después de esa compilación. CMake
queda configurado para copiar ambos archivos a la raíz después de compilar la app.
No se pudo repetir la compilación para validar ese paso automático: el entorno
restringido no accedió al SDK SDL2 y la revisión automática del acceso ampliado
agotó su tiempo de respuesta. Las 557 pruebas corresponden a la validación anterior
del mismo ejecutable; no se han contado como una nueva ejecución.

```powershell
$env:EMULATOR_TEST_BACKEND = 'native'
$env:EMULATOR_BIN = (Resolve-Path 'clothing_app.exe').Path
.\.venv\Scripts\python.exe -B -m pytest tests -p no:cacheprovider -q
cargo test --workspace --locked --release -j 2 --target-dir build/save-review-20260910/target
```

Registros y capturas: `build/save-review-20260910/pytest-final.log`,
`rust-workspace-final.log`, `build-nds-fix.log`, `games/*/probe.log`,
`games/gbc/reopened-battery.png`, `games/gba/reopened-battery.png` y
`games/nds/reopen-communication-error.png`.
