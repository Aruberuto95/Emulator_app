# Historial de errores y soluciones — Emulador GBA/GBC

_Rama `feat/building_brach` · última actualización: 2026-07-02 (día en que se solucionó el audio)_

Resumen sencillo de los problemas que fuimos encontrando y cómo se arreglaron. Cada uno está
descrito con las palabras que usaste cuando lo hablamos.

---

## 1. Pantalla negra (no se veía nada)

- **Lo que dijiste:** el juego mostraba **pantalla negra, sin imagen ni sonido**; pediste que
  "sondea si faltaron instrucciones".
- **Qué pasaba:** el procesador del GBA (ARM7TDMI) estaba casi **vacío** — solo entendía como el
  5% de las instrucciones. El juego arrancaba y se moría enseguida.
- **Cómo se arregló:** se programó el procesador **completo** (ARM + THUMB) y se corrigió el
  arranque. Eso destapó 4 fallos de periféricos que también bloqueaban la imagen (el contador de
  línea VCOUNT congelado, los timers, el vector de interrupción vacío y los registros de
  interrupción desincronizados). También se corrigió el estado inicial de la pantalla (DISPCNT en
  "Forced Blank") para que el juego no se quedara colgado esperando el VBlank.
- **Resultado:** Emerald arranca y muestra el logo verde de Game Freak. ✅

---

## 2. Colores/logo corruptos y crash del driver de sonido

- **Lo que dijiste:** los colores y el logo no salían bien al arrancar.
- **Qué pasaba:** una función del BIOS (**CpuSet**) tenía **dos bits intercambiados**. Cuando el
  juego copiaba su driver de sonido a memoria, en vez de **copiarlo** lo **rellenaba con basura** →
  el juego saltaba a código corrupto y se caía.
- **Cómo se arregló:** se corrigieron los bits. Ojo: esto afecta a **muchos juegos**, porque CpuSet
  se usa por todos lados (copiar paletas, tiles, OAM, drivers...). ✅

---

## 3. Gráficos garabateados en la intro

- **Lo que dijiste:** los gráficos de la intro se veían **garabateados / mal**; la consola
  spammeaba "Unhandled BIOS SWI call: 0x0E".
- **Qué pasaba:** faltaban funciones del BIOS para **rotar/escalar** (afín) y faltaban efectos del
  PPU: **transparencias, ventanas y mosaico**.
- **Cómo se arregló:** se implementaron esas funciones (0x0E / 0x0F) y los efectos de
  mezcla/ventanas/mosaico. Se quitó el spam del SWI 0x0E.
- **Pendiente honesto:** queda un resto de garabato causado por **datos de tiles corruptos en
  VRAM** (más arriba en la cadena, no en el compositor). Aún por investigar. ⚠️

---

## 4. Fast-forward (adelantar) no hacía nada

- **Lo que dijiste:** "**4x exactamente igual**" — poner 4x se veía idéntico a 1x.
- **Qué pasaba:** tres cosas juntas:
  1. Un límite interno de instrucciones **no escalaba** con la velocidad.
  2. Estabas corriendo la build **Debug** (demasiado lenta).
  3. El motor procesaba todo **instrucción por instrucción** (muy caro).
- **Cómo se arregló:** se corrigió el límite, se pasó a build **Release**, y se hizo un
  **planificador por lotes** (procesa en bloques hasta el siguiente evento).
- **Resultado:** 4x ahora llega a ~**5x** real. El GBC (Crystal) a 4x va perfecto. ✅

---

## 5. Sonido del GBA — el problema largo (varias rondas)

Este costó varias vueltas. Lo que se fue arreglando en el camino:

| Lo que se escuchaba | Qué era | Cómo se arregló |
|---|---|---|
| Completamente **mudo** | El DMA que rellena el FIFO de sonido no funcionaba | Se arregló el DMA del FIFO + el registro SOUNDBIAS |
| **Distorsionado** | Faltaban los canales PSG, había recorte (clipping) y offset DC | Se añadió el PSG, se ajustaron volúmenes y se quitó el DC |
| **"Robótico" a 1x** | Estabas corriendo un **.exe viejo (Debug rancio)** | Usar siempre la build Release fresca |
| **"Leve distorsión + ecos"** | Faltaba filtrar los agudos (band-limiting) | Interpolación + filtro pasa-bajos |

### La ronda final (la que lo solucionó)

- **Lo que dijiste (esta última vez):** "se siente **más rápido y distorsionado** el audio y **no
  se escucha toda la música de la intro, hay secciones mudas**."
- **La causa REAL:** **no era el audio.** El sistema de audio ya estaba bien. El problema estaba en
  el **PPU**: disparaba la interrupción de **VBlank 68 veces por frame** en vez de **1 sola vez**.
  Como el motor de música del juego (MP2K) corre una vez por cada VBlank, corría **3 a 11 veces por
  frame** → las canciones iban **aceleradas** y se **acababan antes de tiempo** → por eso se sentía
  más rápido y quedaban ~**23 segundos mudos** en medio de la intro.
- **Cómo se arregló:** **un solo cambio** en el PPU — disparar el VBlank **solo una vez por frame**
  (en el borde de entrada a la línea 160), igual que ya se hacía con el HBlank justo al lado.
- **Resultado:** música **continua** como debe ser, **tempo correcto**, **sin secciones mudas**.
  Verificado: los VBlank pasaron de 3–11 por frame a **1.00**, y el silencio en la intro bajó de
  **30.4 s a 3.2 s** (solo queda el fundido inicial del logo, que es normal). ✅
- **Bonus:** como el VBlank se disparaba 68 veces, afectaba a **todo** lo que corre por frame (no
  solo el audio) → también mejora el timing de los controles y las animaciones en general.

---

## Nota importante: "los tests pasan" no prueba el juego real

La suite de pytest (`tests/`) corre un **mock en Python**, no el emulador compilado. Que diga "111
passed" solo prueba las tuberías, **no** el CPU/PPU/audio reales. Para verificar de verdad hay que
**compilar Release y correr el .exe**.

## Cómo probar la versión arreglada

```
build\bin\Release\clothing_app.exe
```

(Si reconstruyes: `.venv\Scripts\python.exe run_build_and_test.py` con `CARGO_BUILD_JOBS=2`.)
