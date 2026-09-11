# CMakeLists.txt
cmake_minimum_required(VERSION 3.20)
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

option(EMULATOR_BUILD_FRONTEND "Build the SDL2 application" ON)
set(EMULATOR_BUILD_JOBS "2" CACHE STRING "Maximum concurrent Cargo build jobs (1 or 2)")
if(NOT EMULATOR_BUILD_JOBS MATCHES "^[12]$")
    message(FATAL_ERROR "EMULATOR_BUILD_JOBS must be 1 or 2")
endif()
if(NOT CMAKE_CONFIGURATION_TYPES AND NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release CACHE STRING "Build configuration" FORCE)
endif()

# Respect an explicit SDL2_DIR, otherwise use the maintained local Windows SDK.
if(WIN32 AND NOT SDL2_DIR AND EXISTS "${CMAKE_SOURCE_DIR}/sdl2/SDL2-2.32.10/cmake")
    set(SDL2_DIR "${CMAKE_SOURCE_DIR}/sdl2/SDL2-2.32.10/cmake" CACHE PATH "SDL2 package directory")
endif()
if(EMULATOR_BUILD_FRONTEND)
find_package(SDL2 CONFIG QUIET)
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
    message(FATAL_ERROR "SDL2 is required for the frontend. Install the SDK or configure with -DEMULATOR_BUILD_FRONTEND=OFF for a core-only build.")
endif()
endif()

# Rust workspace configurations
if(DEFINED ENV{CARGO_TARGET_DIR} AND NOT "$ENV{CARGO_TARGET_DIR}" STREQUAL "")
    set(RUST_TARGET_DEFAULT "$ENV{CARGO_TARGET_DIR}")
else()
    set(RUST_TARGET_DEFAULT "${CMAKE_CURRENT_SOURCE_DIR}/target")
endif()
set(RUST_TARGET_DIR "${RUST_TARGET_DEFAULT}" CACHE PATH "Cargo artifact directory")
get_filename_component(RUST_TARGET_DIR "${RUST_TARGET_DIR}" ABSOLUTE BASE_DIR "${CMAKE_CURRENT_SOURCE_DIR}")
set(CARGO_RELEASE_CONFIG "$<CONFIG:Release,RelWithDebInfo,MinSizeRel>")
set(CARGO_PROFILE_DIR "$<IF:${CARGO_RELEASE_CONFIG},release,debug>")

# FFI Header Output Directory
set(CXXBRIDGE_DIR "${CMAKE_BINARY_DIR}/cxxbridge/$<CONFIG>")
foreach(config IN LISTS CMAKE_CONFIGURATION_TYPES CMAKE_BUILD_TYPE)
    file(MAKE_DIRECTORY "${CMAKE_BINARY_DIR}/cxxbridge/${config}")
endforeach()


# Path to static library archive (handle MSVC name differences)
if(WIN32 AND NOT MINGW)
    set(RUST_LIB_NAME "emulator_core.lib")
else()
    set(RUST_LIB_NAME "libemulator_core.a")
endif()
set(RUST_LIB_PATH "${RUST_TARGET_DIR}/${CARGO_PROFILE_DIR}/${RUST_LIB_NAME}")

# Custom target to compile Rust core via Cargo and generate FFI headers
add_custom_target(emulator_core_cargo ALL
    COMMAND "${CARGO_EXECUTABLE}" build --locked --package emulator_core
            --jobs "${EMULATOR_BUILD_JOBS}" --target-dir "${RUST_TARGET_DIR}"
            "$<${CARGO_RELEASE_CONFIG}:--release>"
    # Stage Cargo's own headers on every build, including a warm Cargo cache.
    COMMAND "${CMAKE_COMMAND}" -E make_directory "${CXXBRIDGE_DIR}/rust" "${CXXBRIDGE_DIR}/core/src"
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
            "${RUST_TARGET_DIR}/cxxbridge/rust/cxx.h" "${CXXBRIDGE_DIR}/rust/cxx.h"
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
            "${RUST_TARGET_DIR}/cxxbridge/emulator_core/src/lib.rs.h" "${CXXBRIDGE_DIR}/core/src/lib.rs.h"
    BYPRODUCTS "${RUST_LIB_PATH}" "${CXXBRIDGE_DIR}/rust/cxx.h" "${CXXBRIDGE_DIR}/core/src/lib.rs.h"
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    COMMENT "Compiling Rust Core via Cargo"
    USES_TERMINAL
    VERBATIM
    COMMAND_EXPAND_LISTS
)

# Define imported library for Rust
add_library(emulator_core STATIC IMPORTED GLOBAL)
set_target_properties(emulator_core PROPERTIES
    IMPORTED_LOCATION "${RUST_TARGET_DIR}/debug/${RUST_LIB_NAME}"
    IMPORTED_CONFIGURATIONS "DEBUG;RELEASE;RELWITHDEBINFO;MINSIZEREL"
    IMPORTED_LOCATION_DEBUG "${RUST_TARGET_DIR}/debug/${RUST_LIB_NAME}"
    IMPORTED_LOCATION_RELEASE "${RUST_TARGET_DIR}/release/${RUST_LIB_NAME}"
    IMPORTED_LOCATION_RELWITHDEBINFO "${RUST_TARGET_DIR}/release/${RUST_LIB_NAME}"
    IMPORTED_LOCATION_MINSIZEREL "${RUST_TARGET_DIR}/release/${RUST_LIB_NAME}"
    INTERFACE_INCLUDE_DIRECTORIES "${CXXBRIDGE_DIR}"
)
add_dependencies(emulator_core emulator_core_cargo)

# Add frontend directory if SDL2 is found
if(EMULATOR_BUILD_FRONTEND)
    add_subdirectory(frontend)
endif()
