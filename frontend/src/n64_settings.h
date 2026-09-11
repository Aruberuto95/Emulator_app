#pragma once
#include "input_mapping.h"
#include "core/src/lib.rs.h"
#include <algorithm>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <string>
#include <sstream>
#include "settings_file.h"

// One descriptor per emulated control drives defaults, input and both menu lists.
// A binding names a physical button or one signed half of an axis. Keeping its
// kind separate from its index prevents button/axis collisions during validation.
enum class N64BindingKind { Button, AxisNegative, AxisPositive };
struct N64Binding {
    N64BindingKind kind;
    int index;
    bool operator==(const N64Binding& other) const { return kind == other.kind && index == other.index; }
};
struct N64Control {
    const char* name;
    std::uint16_t mask;
    int x;
    int y;
    SDL_Keycode key;
    N64Binding pad;
};
inline constexpr std::uint16_t n64_button_mask(ffi::N64Button button) {
    return static_cast<std::uint16_t>(button);
}
inline constexpr std::array<N64Control, 18> N64_CONTROLS = {{
    {"STICK UP",    0,  0,  1, SDLK_UP,    {N64BindingKind::AxisNegative, SDL_CONTROLLER_AXIS_LEFTY}},
    {"STICK DOWN",  0,  0, -1, SDLK_DOWN,  {N64BindingKind::AxisPositive, SDL_CONTROLLER_AXIS_LEFTY}},
    {"STICK LEFT",  0, -1,  0, SDLK_LEFT,  {N64BindingKind::AxisNegative, SDL_CONTROLLER_AXIS_LEFTX}},
    {"STICK RIGHT", 0,  1,  0, SDLK_RIGHT, {N64BindingKind::AxisPositive, SDL_CONTROLLER_AXIS_LEFTX}},
    {"A", n64_button_mask(ffi::N64Button::A), 0, 0, SDLK_a,      {N64BindingKind::Button, SDL_CONTROLLER_BUTTON_A}},
    {"B", n64_button_mask(ffi::N64Button::B), 0, 0, SDLK_s,      {N64BindingKind::Button, SDL_CONTROLLER_BUTTON_X}},
    {"L", n64_button_mask(ffi::N64Button::L), 0, 0, SDLK_q,      {N64BindingKind::Button, SDL_CONTROLLER_BUTTON_LEFTSHOULDER}},
    {"R", n64_button_mask(ffi::N64Button::R), 0, 0, SDLK_w,      {N64BindingKind::Button, SDL_CONTROLLER_BUTTON_RIGHTSHOULDER}},
    {"C UP",    n64_button_mask(ffi::N64Button::CUp), 0, 0, SDLK_i, {N64BindingKind::AxisNegative, SDL_CONTROLLER_AXIS_RIGHTY}},
    {"C DOWN",  n64_button_mask(ffi::N64Button::CDown), 0, 0, SDLK_k, {N64BindingKind::AxisPositive, SDL_CONTROLLER_AXIS_RIGHTY}},
    {"START",   n64_button_mask(ffi::N64Button::Start), 0, 0, SDLK_RETURN, {N64BindingKind::Button, SDL_CONTROLLER_BUTTON_START}},
    {"Z",       n64_button_mask(ffi::N64Button::Z), 0, 0, SDLK_z, {N64BindingKind::AxisPositive, SDL_CONTROLLER_AXIS_TRIGGERLEFT}},
    {"C LEFT",  n64_button_mask(ffi::N64Button::CLeft), 0, 0, SDLK_j, {N64BindingKind::AxisNegative, SDL_CONTROLLER_AXIS_RIGHTX}},
    {"C RIGHT", n64_button_mask(ffi::N64Button::CRight), 0, 0, SDLK_l, {N64BindingKind::AxisPositive, SDL_CONTROLLER_AXIS_RIGHTX}},
    {"DPAD UP",    n64_button_mask(ffi::N64Button::DpadUp), 0, 0, SDLK_KP_8, {N64BindingKind::Button, SDL_CONTROLLER_BUTTON_DPAD_UP}},
    {"DPAD DOWN",  n64_button_mask(ffi::N64Button::DpadDown), 0, 0, SDLK_KP_2, {N64BindingKind::Button, SDL_CONTROLLER_BUTTON_DPAD_DOWN}},
    {"DPAD LEFT",  n64_button_mask(ffi::N64Button::DpadLeft), 0, 0, SDLK_KP_4, {N64BindingKind::Button, SDL_CONTROLLER_BUTTON_DPAD_LEFT}},
    {"DPAD RIGHT", n64_button_mask(ffi::N64Button::DpadRight), 0, 0, SDLK_KP_6, {N64BindingKind::Button, SDL_CONTROLLER_BUTTON_DPAD_RIGHT}},
}};

struct N64PortSettings {
    std::array<N64Binding, N64_CONTROLS.size()> bindings{};
    int deadzone = 6000;
    bool enabled = true;
    bool invert_x = false;
    bool invert_y = false;
    bool rumble = true;
    ffi::N64Accessory accessory = ffi::N64Accessory::Auto;
    N64PortSettings() {
        for (size_t i = 0; i < bindings.size(); ++i) bindings[i] = N64_CONTROLS[i].pad;
    }
};
struct N64Settings {
    std::array<SDL_Keycode, N64_CONTROLS.size()> keys{};
    std::array<N64PortSettings, 4> ports{};
    N64Settings() {
        for (size_t i = 0; i < keys.size(); ++i) keys[i] = N64_CONTROLS[i].key;
    }
};

// Import only deliberate legacy customizations. The first twelve descriptors
// retain the old mapping order; unmodified aliases (Select/X/Y) become Z/C keys.
// If a custom key occupies a new default, relocate that default, never the user's key.
inline N64Settings n64_settings_from_legacy(const InputMapping& legacy) {
    N64Settings settings;
    const InputMapping defaults;
    std::array<bool, N64_CONTROLS.size()> customized{};
    for (size_t i = 0; i < MAPPING_KEYS.size(); ++i) {
        customized[i] = legacy.*MAPPING_KEYS[i] != defaults.*MAPPING_KEYS[i];
        if (customized[i]) settings.keys[i] = legacy.*MAPPING_KEYS[i];
    }
    for (size_t i = 0; i < settings.keys.size(); ++i) {
        if (customized[i]) continue;
        if (std::count(settings.keys.begin(), settings.keys.end(), settings.keys[i]) < 2) continue;
        for (SDL_Keycode key = SDLK_a; key <= SDLK_z; ++key) {
            if (std::find(settings.keys.begin(), settings.keys.end(), key) == settings.keys.end()) {
                settings.keys[i] = key;
                break;
            }
        }
    }
    return settings;
}

inline bool valid_n64_binding(N64Binding binding) {
    if (binding.index < 0) return false;
    switch (binding.kind) {
        case N64BindingKind::Button: return binding.index < SDL_CONTROLLER_BUTTON_MAX;
        case N64BindingKind::AxisNegative:
            // SDL triggers are unipolar: a negative trigger can never be pressed.
            return binding.index < SDL_CONTROLLER_AXIS_TRIGGERLEFT;
        case N64BindingKind::AxisPositive: return binding.index < SDL_CONTROLLER_AXIS_MAX;
    }
    return false;
}
inline const char* n64_settings_error(const N64Settings& settings) {
    for (size_t i = 0; i < settings.keys.size(); ++i) {
        if (reserved_input_key(settings.keys[i])) return "KEY RESERVED OR INVALID";
        for (size_t j = i + 1; j < settings.keys.size(); ++j)
            if (settings.keys[i] == settings.keys[j]) return "KEY ALREADY IN USE";
    }
    for (const auto& port : settings.ports) {
        if (port.deadzone < 0 || port.deadzone > 30000) return "INVALID DEAD ZONE";
        if (port.accessory != ffi::N64Accessory::Auto && port.accessory != ffi::N64Accessory::None &&
            port.accessory != ffi::N64Accessory::ControllerPak && port.accessory != ffi::N64Accessory::RumblePak)
            return "INVALID ACCESSORY";
        for (size_t i = 0; i < port.bindings.size(); ++i) {
            if (!valid_n64_binding(port.bindings[i])) return "INVALID CONTROLLER INPUT";
            for (size_t j = i + 1; j < port.bindings.size(); ++j)
                if (port.bindings[i] == port.bindings[j]) return "CONTROLLER INPUT ALREADY IN USE";
        }
    }
    return nullptr;
}
inline std::string n64_binding_name(N64Binding binding) {
    if (!valid_n64_binding(binding)) return "INVALID";
    const char* name = binding.kind == N64BindingKind::Button
        ? SDL_GameControllerGetStringForButton(static_cast<SDL_GameControllerButton>(binding.index))
        : SDL_GameControllerGetStringForAxis(static_cast<SDL_GameControllerAxis>(binding.index));
    return std::string(name ? name : "UNKNOWN") + (binding.kind == N64BindingKind::Button ? "" :
        binding.kind == N64BindingKind::AxisNegative ? " -" : " +");
}
inline const char* n64_accessory_name(ffi::N64Accessory accessory) {
    switch (accessory) {
        case ffi::N64Accessory::Auto: return "AUTO";
        case ffi::N64Accessory::None: return "NONE";
        case ffi::N64Accessory::ControllerPak: return "CONTROLLER PAK";
        case ffi::N64Accessory::RumblePak: return "RUMBLE PAK";
        default: return "INVALID";
    }
}

// Parse a bounded, versioned profile into a candidate. A partial/corrupt file
// never changes live bindings; explicit fields make future migrations deliberate.
inline std::string load_n64_settings(const std::filesystem::path& path, N64Settings& settings) {
    std::error_code ec;
    if (!std::filesystem::exists(path, ec) && !ec) return {};
    if (ec || std::filesystem::file_size(path, ec) > 8192 || ec) return "N64 PROFILE UNREADABLE OR TOO LARGE";
    std::ifstream file(path);
    N64Settings candidate;
    std::string magic;
    int version = 0;
    if (!(file >> magic >> version) || magic != "N64_PROFILE" || version != 1) return "INVALID N64 PROFILE VERSION";
    for (auto& key : candidate.keys) if (!(file >> key)) return "INVALID N64 KEYS";
    for (auto& port : candidate.ports) {
        int accessory = 0;
        if (!(file >> port.enabled >> port.deadzone >> port.invert_x >> port.invert_y >> port.rumble >> accessory)
            || accessory < 0 || accessory > 3) return "INVALID N64 PORT";
        port.accessory = static_cast<ffi::N64Accessory>(accessory);
        for (auto& binding : port.bindings) {
            int kind = 0;
            if (!(file >> kind >> binding.index) || kind < 0 || kind > 2) return "INVALID N64 BINDING";
            binding.kind = static_cast<N64BindingKind>(kind);
        }
    }
    file >> std::ws;
    if (!file.eof()) return "TRAILING N64 PROFILE DATA";
    if (const char* error = n64_settings_error(candidate)) return error;
    settings = candidate;
    return {};
}

inline std::string save_n64_settings(const std::filesystem::path& path, const N64Settings& settings) {
    if (const char* error = n64_settings_error(settings)) return error;
    std::ostringstream file;
    file << "N64_PROFILE 1\n";
    for (auto key : settings.keys) file << key << ' ';
    file << '\n';
    for (const auto& port : settings.ports) {
        file << port.enabled << ' ' << port.deadzone << ' ' << port.invert_x << ' ' << port.invert_y
             << ' ' << port.rumble << ' ' << static_cast<int>(port.accessory) << '\n';
        for (auto binding : port.bindings) file << static_cast<int>(binding.kind) << ' ' << binding.index << '\n';
    }
    return replace_settings_file(path, file.str());
}
