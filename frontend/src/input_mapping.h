#pragma once
#include <SDL.h>
#include <array>
#include <iomanip>
#include <sstream>
#include <string>

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

inline constexpr std::array<const char*, 12> MAPPING_NAMES = {
    "UP", "DOWN", "LEFT", "RIGHT", "A", "B", "L", "R", "X", "Y", "START", "SELECT",
};

inline bool reserved_input_key(SDL_Keycode key) {
    const bool valid = (key >= 32 && key <= 126) || key == SDLK_RETURN ||
        key == SDLK_TAB || key == SDLK_BACKSPACE || key == SDLK_DELETE ||
        ((key & SDLK_SCANCODE_MASK) != 0 &&
         (key & ~SDLK_SCANCODE_MASK) > 0 && (key & ~SDLK_SCANCODE_MASK) < SDL_NUM_SCANCODES);
    return !valid || key == SDLK_UNKNOWN ||
        key == SDLK_ESCAPE || key == SDLK_F2 || key == SDLK_F5 || key == SDLK_F9 ||
        (key >= SDLK_0 && key <= SDLK_9);
}

inline const char* input_mapping_error(const InputMapping& mapping) {
    for (size_t i = 0; i < MAPPING_KEYS.size(); ++i) {
        const auto key = mapping.*MAPPING_KEYS[i];
        if (reserved_input_key(key)) {
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

// Read the small legacy JSON object without accepting numeric prefixes or
// applying half of a corrupt file. Missing fields retain historical defaults.
inline const char* parse_input_mappings(const std::string& text, InputMapping& mapping) {
    if (text.size() > 8192) return "INPUT PROFILE TOO LARGE";
    std::istringstream file(text);
    InputMapping candidate;
    std::array<bool, MAPPING_KEYS.size()> seen{};
    char delimiter = 0;
    if (!(file >> delimiter) || delimiter != '{') return "INVALID INPUT PROFILE";
    file >> std::ws;
    if (file.peek() != '}') {
        for (;;) {
            std::string name;
            SDL_Keycode key = SDLK_UNKNOWN;
            if (file.peek() != '"' || !(file >> std::quoted(name) >> delimiter) || delimiter != ':' || !(file >> key))
                return "INVALID INPUT MAPPING";
            size_t index = 0;
            while (index < MAPPING_NAMES.size() && name != MAPPING_NAMES[index]) ++index;
            if (index == MAPPING_NAMES.size() || seen[index]) return "UNKNOWN OR DUPLICATE INPUT MAPPING";
            seen[index] = true;
            candidate.*MAPPING_KEYS[index] = key;
            if (!(file >> delimiter)) return "INCOMPLETE INPUT PROFILE";
            if (delimiter == '}') break;
            if (delimiter != ',') return "INVALID INPUT SEPARATOR";
            file >> std::ws;
        }
    } else file.get();
    file >> std::ws;
    if (!file.eof()) return "TRAILING INPUT PROFILE DATA";
    if (const char* error = input_mapping_error(candidate)) return error;
    mapping = candidate;
    return nullptr;
}
