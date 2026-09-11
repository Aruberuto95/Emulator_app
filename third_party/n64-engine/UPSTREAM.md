# Procedencia y mantenimiento del motor N64

El código de CPU, RSP y periféricos procede de
[Gopher64 v1.1.3](https://github.com/gopher64/gopher64/tree/9fcd2835eba781ac6e81170b434690e38d072bbb),
commit `9fcd2835eba781ac6e81170b434690e38d072bbb`, bajo GPL-3.0.
Se conserva su licencia en `LICENSE-GPL-3.0`. No se incorpora su aplicación SDL3,
su interfaz gráfica, red, actualizador ni acceso a archivos del usuario.

El rasterizador vecino, `../parallel-rdp`, procede de
[Themaister/parallel-rdp-standalone](https://github.com/Themaister/parallel-rdp-standalone/tree/388d70f5835b352d841d9d9e5a08c5de01470f41),
commit `388d70f5835b352d841d9d9e5a08c5de01470f41`, bajo MIT; conserva sus licencias
y las cabeceras de sus componentes. Los shaders SPIR-V incluidos evitan depender
de un compilador de shaders durante la construcción o de descargas al jugar.

## Adaptaciones deliberadas

- `machine.rs` es la API embebida: posee una máquina y su renderer, ejecuta hasta
  el siguiente intervalo VI con presupuesto limitado, y devuelve datos tipados.
  La interfaz C++ del proyecto sigue siendo responsable del ritmo, la ventana,
  los mandos, el dispositivo de audio y los archivos.
- `src/device/` mantiene la interpretación MIPS VR4300, cachés, TLB, RSP escalar
  y vectorial, DMA, interrupciones, PIF, RDRAM y cartucho. El RSP ejecuta el
  microcódigo del propio juego; no se sustituye el microcódigo de Conker por otro.
- Las instrucciones SIMD del RSP declaran SSSE3/SSE4.1 y se despachan únicamente
  tras comprobar esas capacidades. No se sube el requisito de CPU del resto de
  consolas. `unsupported.rs` permite compilar el resto de la aplicación en otras
  arquitecturas y devuelve un error explícito al intentar crear una N64.
- `ram.rs` conserva la alineación de 64 KiB exigida por la importación de RDRAM
  en Vulkan utilizando una asignación propietaria normal. Evita liberar una
  asignación alineada con un layout distinto del utilizado al reservarla.
- Las tablas grandes de `memory.rs` viven en el heap: la deserialización original
  podía desbordar la pila de Windows. Sus longitudes se validan al restaurar.
- `ui.rs` convierte la salida AI a PCM estéreo continuo, conservando la fase del
  remuestreo entre bloques. No abre dispositivos, archivos ni conexiones.
- `cpp/renderer.cpp` valida los segmentos FFI, decodifica listas RDP incrementales
  y sincroniza las escrituras de GPU antes de exponer la interrupción DP. La
  configuración de Volk es idéntica en C y C++ para preservar su ABI en Windows.
- `snapshot.rs` guarda CPU, RAM, cachés, RSP, periféricos, RNG, audio y frame. El
  adaptador C++ conserva comandos de estado, TMEM, RAM de cobertura, comandos
  pendientes y contadores de ruido VI/RDP. Se restauran comandos, nunca punteros
  o objetos Vulkan. Las pequeñas extensiones al código MIT exponen exclusivamente
  dichos contadores y la sincronización de escrituras de TMEM.
- `battery.rs` conserva EEPROM, SRAM/Flash y los cuatro Controller Pak en un
  contenedor con checksum. Admite importar los formatos crudos de cartucho.
  La escritura atómica y el confinamiento de rutas pertenecen a `core/src/rom.rs`.
- Se retiraron los módulos no conectados de VRU y Transfer Pak y el limitador
  temporal de Gopher64: el frontend del proyecto ya posee un planificador.

## Actualización

Actualizar primero las revisiones fijadas y comparar **código activo**, conservar
las adaptaciones anteriores, comprobar las licencias y ejecutar las regresiones
del proyecto más los ejemplos `probe` y `verify_resume` con ROM local. No cambiar
el formato de snapshot sin incrementar su versión y probar el rechazo explícito
de estados incompatibles. Los snapshots no contienen la ROM del cartucho.
