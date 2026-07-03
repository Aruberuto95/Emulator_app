# CMakeLists.txt
cmake_minimum_required(VERSION 3.19)
project(clothing_app LANGUAGES C CXX)

# Require C++17
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Match the CRT that Rust's staticlib links against. Rust always uses the dynamic release
# CRT (MD) on MSVC, even for dev builds, so the C++ frontend must too. Otherwise linking the
# Debug frontend against the Rust lib fails with LNK2038 (_ITERATOR_DEBUG_LEVEL / RuntimeLibrary
# mismatch). Forcing MD also keeps _ITERATOR_DEBUG_LEVEL=0 on both sides.
if(MSVC)
    cmake_policy(SET CMP0091 NEW)
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()

# Force output directories
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)
set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)

# Setup search path prefixes for homebrew on macOS
list(APPEND CMAKE_PREFIX_PATH "/opt/homebrew" "/usr/local")

# Locate Cargo
find_program(CARGO_EXECUTABLE cargo REQUIRED)
message(STATUS "Found Cargo: ${CARGO_EXECUTABLE}")

# Locate SDL2
find_package(SDL2 CONFIG)
if(NOT SDL2_FOUND)
    find_package(SDL2)
endif()

if(NOT SDL2_FOUND)
    # Fallback to pkg-config
    find_package(PkgConfig)
    if(PkgConfig_FOUND)
        pkg_check_modules(SDL2 sdl2)
    endif()
endif()

if(SDL2_FOUND OR TARGET SDL2::SDL2)
    message(STATUS "SDL2 found, C++ frontend will be built.")
    set(HAVE_SDL2 TRUE)
    
    # Ensure SDL2::SDL2 target is defined
    if(NOT TARGET SDL2::SDL2)
        if(NOT SDL2_INCLUDE_DIRS AND SDL2_INCLUDE_DIR)
            set(SDL2_INCLUDE_DIRS "${SDL2_INCLUDE_DIR}")
        endif()
        if(NOT SDL2_LIBRARIES AND SDL2_LIBRARY)
            set(SDL2_LIBRARIES "${SDL2_LIBRARY}")
        endif()
        add_library(SDL2::SDL2 INTERFACE IMPORTED)
        set_target_properties(SDL2::SDL2 PROPERTIES
            INTERFACE_INCLUDE_DIRECTORIES "${SDL2_INCLUDE_DIRS}"
            INTERFACE_LINK_LIBRARIES "${SDL2_LIBRARIES}"
        )
    endif()
else()
    message(WARNING "SDL2 not found, C++ frontend will NOT be built.")
    set(HAVE_SDL2 FALSE)
endif()

# Rust workspace configurations
set(RUST_TARGET_DIR "${CMAKE_CURRENT_SOURCE_DIR}/target")
if(CMAKE_BUILD_TYPE STREQUAL "Release" OR CMAKE_BUILD_TYPE STREQUAL "RelWithDebInfo" OR CMAKE_BUILD_TYPE STREQUAL "MinSizeRel")
    set(CARGO_BUILD_FLAG "--release")
    set(CARGO_PROFILE_DIR "release")
else()
    set(CARGO_BUILD_FLAG "")
    set(CARGO_PROFILE_DIR "debug")
endif()

# FFI Header Output Directory
set(CXXBRIDGE_DIR "${CMAKE_BINARY_DIR}/cxxbridge")
file(MAKE_DIRECTORY "${CXXBRIDGE_DIR}")


# Path to static library archive (handle MSVC name differences)
if(WIN32 AND NOT MINGW)
    set(RUST_LIB_NAME "emulator_core.lib")
else()
    set(RUST_LIB_NAME "libemulator_core.a")
endif()
set(RUST_LIB_PATH "${RUST_TARGET_DIR}/${CARGO_PROFILE_DIR}/${RUST_LIB_NAME}")

# Custom target to compile Rust core via Cargo and generate FFI headers
add_custom_target(emulator_core_cargo ALL
    COMMAND ${CMAKE_COMMAND} -E env CXXBRIDGE_OUTPUT_DIR=${CXXBRIDGE_DIR}
            ${CARGO_EXECUTABLE} build ${CARGO_BUILD_FLAG}
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    COMMENT "Compiling Rust Core via Cargo"
    USES_TERMINAL
)

# Define imported library for Rust
add_library(emulator_core STATIC IMPORTED GLOBAL)
set_target_properties(emulator_core PROPERTIES
    IMPORTED_LOCATION "${RUST_LIB_PATH}"
    INTERFACE_INCLUDE_DIRECTORIES "${CXXBRIDGE_DIR}"
)
add_dependencies(emulator_core emulator_core_cargo)

# Add frontend directory if SDL2 is found
if(HAVE_SDL2)
    add_subdirectory(frontend)
endif()
