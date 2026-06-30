import os
import urllib.request
import zipfile
import subprocess
import shutil

def main():
    workspace = os.path.dirname(os.path.abspath(__file__))
    sdl2_dir = os.path.join(workspace, "sdl2")
    zip_path = os.path.join(workspace, "SDL2-devel-2.30.4-VC.zip")
    
    # Try to find cargo
    cargo_path = shutil.which("cargo")
    if not cargo_path:
        user_profile = os.environ.get("USERPROFILE", "")
        cargo_candidate = os.path.join(user_profile, ".cargo", "bin", "cargo.exe")
        if os.path.exists(cargo_candidate):
            cargo_path = cargo_candidate
            bin_dir = os.path.dirname(cargo_path)
            if bin_dir not in os.environ["PATH"]:
                os.environ["PATH"] = bin_dir + os.pathsep + os.environ["PATH"]
                print(f"Added Cargo bin to PATH: {bin_dir}")

    if not os.path.exists(sdl2_dir):
        os.makedirs(sdl2_dir)
        
    url = "https://github.com/libsdl-org/SDL/releases/download/release-2.30.4/SDL2-devel-2.30.4-VC.zip"
    
    # Download if not already extracted
    sdl2_config_dir = os.path.join(sdl2_dir, "SDL2-2.30.4")
    if not os.path.exists(sdl2_config_dir):
        print(f"Downloading SDL2 from {url}...")
        try:
            urllib.request.urlretrieve(url, zip_path)
            print("Download complete.")
        except Exception as e:
            print(f"Failed to download: {e}")
            return

        print("Extracting ZIP file...")
        try:
            with zipfile.ZipFile(zip_path, 'r') as zip_ref:
                zip_ref.extractall(sdl2_dir)
            print("Extraction complete.")
        except Exception as e:
            print(f"Failed to extract: {e}")
            return
            
        # Remove ZIP
        if os.path.exists(zip_path):
            os.remove(zip_path)
    else:
        print(f"SDL2 already exists at {sdl2_config_dir}")
    
    # Configure CMake
    build_dir = os.path.join(workspace, "build")
    # Clean previous CMakeCache to force search
    cache_file = os.path.join(build_dir, "CMakeCache.txt")
    if os.path.exists(cache_file):
        os.remove(cache_file)
        print("Cleared CMakeCache.txt")
        
    sdl2_cmake_dir = os.path.join(sdl2_config_dir, "cmake")
    cmake_cmd = [
        "cmake",
        "-B", "build",
        "-S", ".",
        "-DCMAKE_BUILD_TYPE=Release",
        f"-DSDL2_DIR={sdl2_cmake_dir.replace(chr(92), '/')}"
    ]
    
    # Look for CMake in Program Files
    cmake_path = shutil.which("cmake")
    if not cmake_path:
        cmake_candidates = [
            r"C:\Program Files\CMake\bin\cmake.exe",
            r"C:\Program Files (x86)\CMake\bin\cmake.exe"
        ]
        for candidate in cmake_candidates:
            if os.path.exists(candidate):
                cmake_path = candidate
                break
    if cmake_path:
        cmake_cmd[0] = cmake_path
        
    print(f"Configuring project: {' '.join(cmake_cmd)}")
    subprocess.run(cmake_cmd, check=True, shell=True)
    
    # Build project in Release
    build_cmd = [cmake_cmd[0], "--build", "build", "--config", "Release", "-j", "2"]
    print(f"Building project: {' '.join(build_cmd)}")
    subprocess.run(build_cmd, check=True, shell=True)
    
    # Copy SDL2.dll to binary directories so it can run
    dll_source = os.path.join(sdl2_config_dir, "lib", "x64", "SDL2.dll")
    dll_dest_release = os.path.join(build_dir, "bin", "Release", "SDL2.dll")
    dll_dest_debug = os.path.join(build_dir, "bin", "Debug", "SDL2.dll")
    dll_dest_bin = os.path.join(build_dir, "bin", "SDL2.dll")
    
    os.makedirs(os.path.dirname(dll_dest_release), exist_ok=True)
    os.makedirs(os.path.dirname(dll_dest_debug), exist_ok=True)
    os.makedirs(os.path.dirname(dll_dest_bin), exist_ok=True)
    
    if os.path.exists(dll_source):
        shutil.copy2(dll_source, dll_dest_release)
        shutil.copy2(dll_source, dll_dest_debug)
        shutil.copy2(dll_source, dll_dest_bin)
        print("Copied SDL2.dll to binary directories.")

if __name__ == "__main__":
    main()
