# Emulator_app

Emulador GBC, GBA y Nintendo DS con núcleo Rust y aplicación C++17/SDL2.
Proyecto personal: "Repository for the sole purpose of doing an app for my future wife".

La compatibilidad depende del juego. NDS incluye arranque HLE, doble pantalla,
audio y entrada táctil. El JIT ARM9/ARM7 funciona en Windows x64; otros hosts usan
el intérprete. Los multiplicadores de velocidad expresan una petición y no una
velocidad garantizada.

## Compilar y validar en Windows

Requiere Rust/Cargo, CMake 3.20+, MSVC Build Tools con C++ y Windows SDK,
y Python 3.10+. Entorno local verificado: Rust 1.96.1, CMake 4.3.3 y Python 3.13.
El lockfile incluye dependencias que requieren Rust 1.85 como mínimo; no se ha
validado aquí una matriz de versiones anteriores.

Desde la raíz del repositorio:

```powershell
python -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements-test.txt
.\.venv\Scripts\python.exe install_sdl2.py --download-only
.\.venv\Scripts\python.exe run_build_and_test.py
.\clothing_app.exe
```

El instalador descarga SDL2 2.32.10 y comprueba su SHA-256. El validador ejecuta
pruebas Rust, compila Release con hasta dos trabajos, verifica los controles y
prueba el ejecutable real mediante pytest. Una compilación sin SDL2 falla de
forma explícita. No instala dependencias ni modifica las fuentes de los tests.

Cada compilación de la aplicación actualiza `clothing_app.exe` y `SDL2.dll` en
la raíz del proyecto. Use Release para jugar y cierre la app antes de actualizarla.

Las ROMs y partidas del usuario no son necesarias para las pruebas automáticas.

- [Uso, controles y partidas](GUIA_DE_USO.md)
- [Arquitectura y carpetas](PROJECT.md)
- [Cómo validar y qué cubren las pruebas](TEST_INFRA.md)
- [Resultados y límites de la última validación](TEST_READY.md)
- [Avance rápido NDS: medición real y resultados](docs/NDS_FAST_FORWARD.md)
- [Compilación portable con perfiles PGO](docs/NDS_PGO.md)
- [Registros históricos](docs/history/README.md)
