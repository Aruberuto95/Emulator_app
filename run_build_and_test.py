"""Build and validate the native emulator; install requirements-test.txt beforehand."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys

WORKSPACE = Path(__file__).resolve().parent


def run(command, env=None, timeout=900):
    print("[BUILD_AND_TEST] " + subprocess.list2cmdline([str(arg) for arg in command]), flush=True)
    subprocess.run([str(arg) for arg in command], cwd=WORKSPACE, env=env, check=True, timeout=timeout)


def find_tool(name):
    found = shutil.which(name)
    candidates = ([Path.home() / ".cargo" / "bin" / "cargo.exe"] if name == "cargo" else
                  [Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "CMake" / "bin" / "cmake.exe"])
    if not found:
        found = next((str(p) for p in candidates if p.is_file()), None)
    if not found:
        raise RuntimeError(f"Required tool not found: {name}")
    return found


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-dir", type=Path, default=WORKSPACE / "build")
    parser.add_argument("--config", choices=("Debug", "Release", "RelWithDebInfo", "MinSizeRel"), default="Release")
    parser.add_argument("--jobs", type=int, choices=(1, 2), default=2)
    parser.add_argument("--build-timeout", type=float, default=900)
    parser.add_argument("--test-timeout", type=float, default=180)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--skip-rust-tests", action="store_true")
    parser.add_argument("--emulator-bin", type=Path)
    args = parser.parse_args(argv)
    if args.build_timeout <= 0 or args.test_timeout <= 0:
        parser.error("timeouts must be positive")
    build_dir = (WORKSPACE / args.build_dir).resolve()
    try:
        # Fail early with setup guidance; validation never changes its own dependencies.
        try:
            import pytest  # noqa: F401
        except ImportError as error:
            raise RuntimeError("Install test dependencies with python -m pip install -r requirements-test.txt") from error
        if not args.skip_rust_tests:
            run([find_tool("cargo"), "test", "--workspace", "--locked", "-j", args.jobs]
                + (["--release"] if args.config != "Debug" else []), timeout=args.build_timeout)
        if not args.skip_build:
            cmake = find_tool("cmake")
            run([cmake, "-S", WORKSPACE, "-B", build_dir,
                 f"-DCMAKE_BUILD_TYPE={args.config}", f"-DEMULATOR_BUILD_JOBS={args.jobs}"], timeout=args.build_timeout)
            run([cmake, "--build", build_dir, "--config", args.config,
                 "--target", "clothing_app", "input_mapping_tests", "frame_pacing_tests",
                 "--parallel", args.jobs], timeout=args.build_timeout)
        suffix = ".exe" if sys.platform == "win32" else ""
        candidates = ([(WORKSPACE / args.emulator_bin).resolve()] if args.emulator_bin else [
            build_dir / "bin" / args.config / ("clothing_app" + suffix),
            build_dir / "bin" / ("clothing_app" + suffix),
        ])
        binary = next((p for p in candidates if p.is_file() and p.suffix.lower() != ".py"), None)
        if binary is None:
            raise RuntimeError("Native clothing_app binary missing; SDL2 and the frontend build are required")
        for name in ("input_mapping_tests", "frame_pacing_tests"):
            native_test = binary.parent / (name + suffix)
            if not native_test.is_file():
                raise RuntimeError(f"Required native regression binary missing: {native_test}")
            run([native_test], timeout=args.test_timeout)
        env = os.environ.copy()
        env["EMULATOR_BIN"] = str(binary)
        env["EMULATOR_TEST_BACKEND"] = "native"
        print(f"[BUILD_AND_TEST] Integration target: {binary}", flush=True)
        run([sys.executable, "-m", "pytest", "tests/", "-ra"], env=env, timeout=args.test_timeout)
        print("[BUILD_AND_TEST] Native build and requested validation passed.", flush=True)
        return 0
    except (OSError, RuntimeError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        print(f"[BUILD_AND_TEST] Verification failed: {error}", file=sys.stderr)
        return error.returncode if isinstance(error, subprocess.CalledProcessError) else 1


if __name__ == "__main__":
    sys.exit(main())
