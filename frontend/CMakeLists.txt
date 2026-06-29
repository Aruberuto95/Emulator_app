# frontend/CMakeLists.txt

add_executable(clothing_app src/main.cpp)

# Include target cxxbridge headers (for "core/src/lib.rs.h" and "rust/cxx.h")
target_include_directories(clothing_app PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/src"
    "${CXXBRIDGE_DIR}"
)

# Link C++ executable with Rust core static lib, SDL2, and macOS system frameworks
target_link_libraries(clothing_app PRIVATE
    emulator_core
    SDL2::SDL2
)

# On macOS, Rust's stdlib requires CoreFoundation and Security frameworks
if(APPLE)
    target_link_libraries(clothing_app PRIVATE
        "-framework CoreFoundation"
        "-framework Security"
    )
endif()
