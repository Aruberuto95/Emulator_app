#pragma once
#include <SDL.h>
#include <array>

struct InputMapping {
    SDL_Keycode up = SDLK_UP;
    SDL_Keycode down = SDLK_DOWN;
    SDL_Keycode left = SDLK_LEFT;
    SDL_Keycode right = SDLK_RIGHT;
    SDL_Keycode a = SDLK_a;
    SDL_Keycode b = SDLK_s;
    SDL_Keycode l = SDLK_q;
    SDL_Keycode r = SDLK_w;
    SDL_Keycode select = SDLK_c;
    SDL_Keycode start = SDLK_RETURN;
    SDL_Keycode x = SDLK_x;
    SDL_Keycode y = SDLK_y;
};

inline constexpr std::array<SDL_Keycode InputMapping::*, 12> MAPPING_KEYS = {
    &InputMapping::up, &InputMapping::down, &InputMapping::left, &InputMapping::right,
    &InputMapping::a, &InputMapping::b, &InputMapping::l, &InputMapping::r,
    &InputMapping::x, &InputMapping::y, &InputMapping::start, &InputMapping::select,
};

inline const char* input_mapping_error(const InputMapping& mapping) {
    for (size_t i = 0; i < MAPPING_KEYS.size(); ++i) {
        const auto key = mapping.*MAPPING_KEYS[i];
        if (key == SDLK_UNKNOWN || key == SDLK_ESCAPE || key == SDLK_F2 ||
            key == SDLK_F5 || key == SDLK_F9 || (key >= SDLK_0 && key <= SDLK_9)) {
            return "KEY RESERVED - CHOOSE ANOTHER";
        }
        for (size_t j = i + 1; j < MAPPING_KEYS.size(); ++j) {
            if (key == mapping.*MAPPING_KEYS[j]) return "KEY ALREADY IN USE";
        }
    }
    return nullptr;
}

inline const char* rebind_input(InputMapping& mapping, int row, SDL_Keycode key) {
    if (row < 0 || static_cast<size_t>(row) >= MAPPING_KEYS.size()) return "INVALID CONTROL";
    InputMapping candidate = mapping;
    candidate.*MAPPING_KEYS[row] = key;
    if (const char* error = input_mapping_error(candidate)) return error;
    mapping = candidate;
    return nullptr;
}
