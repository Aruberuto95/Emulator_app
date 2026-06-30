# Updated build and test script
import os
import sys
import shutil
import subprocess

def log(msg):
    print(f"[BUILD_AND_TEST] {msg}")
    sys.stdout.flush()

def search_toolchain():
    log(f"Current PATH: {os.environ.get('PATH', '')}")
    
    cmake_path = shutil.which("cmake")
    cargo_path = shutil.which("cargo")
    log(f"Initial shutil.which check - cmake: {cmake_path}, cargo: {cargo_path}")

    # Look for Cargo in user profile
    if not cargo_path:
        user_profile = os.environ.get("USERPROFILE", "")
        cargo_candidate = os.path.join(user_profile, ".cargo", "bin", "cargo.exe")
        if os.path.exists(cargo_candidate):
            cargo_path = cargo_candidate
            log(f"Found cargo at candidate path: {cargo_path}")
            bin_dir = os.path.dirname(cargo_path)
            if bin_dir not in os.environ["PATH"]:
                os.environ["PATH"] = bin_dir + os.pathsep + os.environ["PATH"]
                log(f"Added {bin_dir} to PATH")

    # Look for CMake in Program Files
    if not cmake_path:
        cmake_candidates = [
            r"C:\Program Files\CMake\bin\cmake.exe",
            r"C:\Program Files (x86)\CMake\bin\cmake.exe"
        ]
        for candidate in cmake_candidates:
            if os.path.exists(candidate):
                cmake_path = candidate
                log(f"Found cmake at candidate path: {cmake_path}")
                bin_dir = os.path.dirname(cmake_path)
                if bin_dir not in os.environ["PATH"]:
                    os.environ["PATH"] = bin_dir + os.pathsep + os.environ["PATH"]
                    log(f"Added {bin_dir} to PATH")
                break

    # Recheck
    cmake_path = shutil.which("cmake")
    cargo_path = shutil.which("cargo")
    log(f"Final toolchain paths - cmake: {cmake_path}, cargo: {cargo_path}")
    return cmake_path, cargo_path

def run_command(cmd, cwd=None):
    log(f"Running command: {' '.join(cmd)} in {cwd or 'current directory'}")
    executable = shutil.which(cmd[0])
    if executable:
        cmd[0] = executable
        log(f"Resolved command executable to: {executable}")
    else:
        log(f"Warning: Could not resolve executable for {cmd[0]}")
        
    is_windows = sys.platform.startswith('win')
    process = subprocess.Popen(
        cmd, 
        stdout=subprocess.PIPE, 
        stderr=subprocess.STDOUT, 
        text=True, 
        cwd=cwd,
        shell=is_windows
    )
    output_lines = []
    while True:
        line = process.stdout.readline()
        if not line and process.poll() is not None:
            break
        if line:
            print(line, end="")
            sys.stdout.flush()
            output_lines.append(line)
    rc = process.poll()
    if rc != 0:
        raise subprocess.CalledProcessError(rc, cmd, "".join(output_lines))
    return rc

def patch_test_files():
    workspace_root = os.path.abspath(os.path.dirname(__file__))
    safe_workspace_root = workspace_root.replace('\\', '/')
    test_files = [
        os.path.join(workspace_root, "tests", "test_e2e.py"),
        os.path.join(workspace_root, "tests", "test_adversarial.py")
    ]
    backups = {}
    for tf in test_files:
        if os.path.exists(tf):
            log(f"Reading {tf} for patching...")
            with open(tf, 'r', encoding='utf-8') as f:
                content = f.read()
            backups[tf] = content
            target_str = 'workspace_dir = "/Users/a.rudolph/Proyectos Albert/clothing_app"'
            replacement_str = f'workspace_dir = "{safe_workspace_root}"'
            if target_str in content:
                patched_content = content.replace(target_str, replacement_str)
                with open(tf, 'w', encoding='utf-8') as f:
                    f.write(patched_content)
                log(f"Successfully patched {tf}")
            else:
                log(f"Warning: could not find target string in {tf}")
    return backups

def restore_test_files(backups):
    for tf, original_content in backups.items():
        try:
            with open(tf, 'w', encoding='utf-8') as f:
                f.write(original_content)
            log(f"Restored {tf} to original state.")
        except Exception as e:
            log(f"Failed to restore {tf}: {e}")

def main():
    backups = patch_test_files()
    try:
        # Search and set up toolchain
        cmake_ok, cargo_ok = search_toolchain()
        if not cmake_ok or not cargo_ok:
            log("Warning: Toolchain could not be fully resolved.")
            
        # Try to install pytest in .venv
        python_bin = os.path.join(".venv", "Scripts", "python.exe")
        log("Checking virtual environment pytest package installation...")
        try:
            # Install pytest, using cache if offline
            run_command([python_bin, "-m", "pip", "install", "pytest"])
        except Exception as e:
            log(f"Installing pytest in venv failed/skipped: {e}")

        # Configure build
        log("Configuring CMake build...")
        run_command(["cmake", "-B", "build", "-S", ".", "-DCMAKE_BUILD_TYPE=Debug"])

        # Build targets
        log("Compiling emulator target...")
        run_command(["cmake", "--build", "build", "-j", "2"])

        # Run tests using the virtualenv pytest
        log("Running test suite using pytest...")
        pytest_bin = os.path.join(".venv", "Scripts", "pytest.exe")
        if not os.path.exists(pytest_bin):
            pytest_bin = os.path.join(".venv", "Scripts", "pytest")

        if os.path.exists(pytest_bin):
            test_cmd = [pytest_bin, "tests/"]
        else:
            test_cmd = [python_bin, "-m", "pytest", "tests/"]

        run_command(test_cmd)
        log("All build and test verification passed successfully!")
    except subprocess.CalledProcessError as e:
        log(f"Verification failed! Command exited with code {e.returncode}")
        sys.exit(e.returncode)
    except Exception as e:
        log(f"An unexpected error occurred: {e}")
        sys.exit(1)
    finally:
        log("Restoring test files...")
        restore_test_files(backups)

if __name__ == "__main__":
    main()
