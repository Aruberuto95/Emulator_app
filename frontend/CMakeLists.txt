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

if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
    target_link_libraries(clothing_app PRIVATE
        pthread
        dl
        rt
        m
    )
endif()

if(WIN32)
    target_link_libraries(clothing_app PRIVATE
        ws2_32
        userenv
        bcrypt
        ntdll
    )
endif()

# Small native regression target: validates the same bindings used by the UI.
add_executable(input_mapping_tests tests/input_mapping_tests.cpp)
target_include_directories(input_mapping_tests PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")
target_link_libraries(input_mapping_tests PRIVATE SDL2::SDL2)

if(WIN32 AND TARGET SDL2::SDL2)
    get_target_property(SDL2_TARGET_TYPE SDL2::SDL2 TYPE)
    if(SDL2_TARGET_TYPE STREQUAL "SHARED_LIBRARY")
        foreach(app_target clothing_app input_mapping_tests)
            add_custom_command(TARGET ${app_target} POST_BUILD
                COMMAND ${CMAKE_COMMAND} -E copy_if_different
                    "$<TARGET_FILE:SDL2::SDL2>" "$<TARGET_FILE_DIR:${app_target}>"
                VERBATIM)
        endforeach()
    endif()
endif()
