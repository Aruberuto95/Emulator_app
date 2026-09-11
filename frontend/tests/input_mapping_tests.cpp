#define SDL_MAIN_HANDLED
#include "input_mapping.h"
#include <iostream>

static int failures = 0;
static void check(bool condition, const char* message) {
    if (!condition) { std::cerr << message << "\n"; ++failures; }
}
int main() {
    InputMapping mapping;
    check(!input_mapping_error(mapping), "default mappings must be valid");
    for (SDL_Keycode key : {SDLK_UNKNOWN, SDLK_ESCAPE, SDLK_F2, SDLK_F5, SDLK_F9,
                            SDLK_0, SDLK_1, SDLK_9, SDLK_s}) {
        check(rebind_input(mapping, 4, key) != nullptr, "invalid A binding accepted");
        check(mapping.a == SDLK_a, "rejected binding changed the mapping");
    }
    check(!rebind_input(mapping, 4, SDLK_z), "valid A binding rejected");
    check(mapping.a == SDLK_z && mapping.b == SDLK_s, "valid binding changed another control");
    check(!rebind_input(mapping, 4, SDLK_z), "rebinding to the same key must work");
    check(rebind_input(mapping, -1, SDLK_t) != nullptr, "negative control accepted");
    check(rebind_input(mapping, 12, SDLK_t) != nullptr, "out-of-range control accepted");
    InputMapping corrupt = mapping;
    corrupt.b = mapping.a;
    check(input_mapping_error(corrupt) != nullptr, "duplicate loaded mapping accepted");
    corrupt = mapping;
    corrupt.a = SDLK_F5;
    check(input_mapping_error(corrupt) != nullptr, "reserved loaded mapping accepted");
    InputMapping restored;
    check(!parse_input_mappings("{\"A\":116}", restored) && restored.a == SDLK_t,
          "partial legacy profile should retain defaults");
    for (const auto* text : {"{\"A\":116x}", "{\"A\":116,\"A\":117}",
                            "{\"A\":116,}", "{\"A\":116}trailing", "{\"A\":116", "{\"A\":27}"}) {
        check(parse_input_mappings(text, restored) != nullptr, "corrupt legacy profile accepted");
        check(restored.a == SDLK_t, "corrupt legacy profile changed live input");
    }
    return failures ? 1 : 0;
}
