# Componentes incorporados para Nintendo 64

| Componente | Revisión fijada | Licencia conservada |
|---|---|---|
| CPU/RSP/periféricos adaptados de Gopher64 v1.1.3 | `9fcd2835eba781ac6e81170b434690e38d072bbb` | `third_party/n64-engine/LICENSE-GPL-3.0` |
| paraLLEl-RDP standalone | `388d70f5835b352d841d9d9e5a08c5de01470f41` | `third_party/parallel-rdp/LICENSE`, cabeceras de cada componente |

El núcleo Rust incorporado está bajo GPL-3.0. La integración lo enlaza dentro de
la aplicación; cualquier redistribución debe conservar su licencia y cumplir sus
condiciones, incluyendo las relativas al código fuente correspondiente. Este
archivo no cambia la autoría de los archivos preexistentes del proyecto.

`third_party/n64-engine/UPSTREAM.md` describe el origen y las modificaciones.
`Cargo.lock` fija las dependencias Rust restantes, que conservan sus propias
licencias. Ninguna ROM comercial ni captura del juego forma parte del código
incorporado; las evidencias locales se generan dentro de `build/`.
