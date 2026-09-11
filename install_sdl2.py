"""Install the pinned SDL2 Windows SDK; optionally build the Release frontend."""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parent
SDL_VERSION = "2.32.10"
SDL_SHA256 = "af347939395a58b365846aaea27391e69f9ec9d4dd650d6ac40802159b418a6e"
SDL_ARCHIVE = f"SDL2-devel-{SDL_VERSION}-VC.zip"
SDL_URL = f"https://github.com/libsdl-org/SDL/releases/download/release-{SDL_VERSION}/{SDL_ARCHIVE}"


def install_sdk():
    if sys.platform != "win32":
        raise RuntimeError("This installer provides the Windows SDK. Install SDL2 with your system package manager on other platforms.")
    sdk_root = ROOT / "sdl2"
    sdk_dir = sdk_root / f"SDL2-{SDL_VERSION}"
    required = ["include/SDL.h", "lib/x64/SDL2.dll", "cmake/sdl2-config.cmake"]
    if sdk_dir.exists():
        if not all((sdk_dir / name).is_file() for name in required):
            raise RuntimeError(f"Incomplete SDL2 SDK at {sdk_dir}; move it aside before retrying.")
        print(f"SDL2 {SDL_VERSION}: {sdk_dir}", flush=True)
        return sdk_dir
    sdk_root.mkdir(exist_ok=True)
    # The temporary extraction lives inside sdl2; existing SDKs remain available.
    with tempfile.TemporaryDirectory(prefix="download-", dir=sdk_root) as temp:
        archive = Path(temp) / SDL_ARCHIVE
        print(f"Downloading SDL2 {SDL_VERSION}", flush=True)
        with urllib.request.urlopen(SDL_URL, timeout=60) as response, archive.open("wb") as output:
            shutil.copyfileobj(response, output, length=1024 * 1024)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if digest != SDL_SHA256:
            raise RuntimeError(f"SDL2 archive checksum mismatch: {digest}")
        with zipfile.ZipFile(archive) as bundle:
            bundle.extractall(temp)
        unpacked = Path(temp) / f"SDL2-{SDL_VERSION}"
        if not all((unpacked / name).is_file() for name in required):
            raise RuntimeError("The SDL2 archive is missing required SDK files")
        unpacked.rename(sdk_dir)
    print(f"Installed SDL2 {SDL_VERSION}: {sdk_dir}", flush=True)
    return sdk_dir


def find_tool(name, candidates):
    found = shutil.which(name)
    if found:
        return found
    for candidate in candidates:
        if candidate.is_file():
            os.environ["PATH"] = str(candidate.parent) + os.pathsep + os.environ.get("PATH", "")
            return str(candidate)
    raise RuntimeError(f"{name} is required and was not found")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--download-only", action="store_true", help="Install the SDK without compiling")
    parser.add_argument("--build-dir", type=Path, default=ROOT / "build")
    args = parser.parse_args()
    try:
        sdk = install_sdk()
        if args.download_only:
            return 0
        find_tool("cargo", [Path.home() / ".cargo/bin/cargo.exe"])
        cmake = find_tool("cmake", [Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "CMake/bin/cmake.exe"])
        build = args.build_dir if args.build_dir.is_absolute() else ROOT / args.build_dir
        commands = [
            [cmake, "-S", str(ROOT), "-B", str(build), "-DCMAKE_BUILD_TYPE=Release", f"-DSDL2_DIR={sdk / 'cmake'}"],
            [cmake, "--build", str(build), "--config", "Release", "--target", "clothing_app", "--parallel", "2"],
        ]
        for command in commands:
            print(subprocess.list2cmdline(command), flush=True)
            subprocess.run(command, cwd=ROOT, check=True, timeout=900)
        return 0
    except (OSError, RuntimeError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        print(f"SDL2 setup failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
