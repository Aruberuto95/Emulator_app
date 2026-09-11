#pragma once
#include "n64_settings.h"
#include <cstdlib>
#include <optional>

struct N64PadState {
    std::array<bool, SDL_CONTROLLER_BUTTON_MAX> buttons{};
    std::array<Sint16, SDL_CONTROLLER_AXIS_MAX> axes{};
};

// Own SDL handles and borrow the live profile: menus and input always agree.
class N64Controllers {
    N64Settings& settings;
    std::array<SDL_GameController*, 4> controllers{};
    std::array<bool, N64_CONTROLS.size()> keyboard{};
    std::array<std::array<bool, N64_CONTROLS.size()>, 4> blocked{};
    std::array<bool, 4> rumbling{};
    std::array<Uint32, 4> rumble_at{};
public:
    explicit N64Controllers(N64Settings& profile) : settings(profile) {}
    N64Controllers(const N64Controllers&) = delete;
    N64Controllers& operator=(const N64Controllers&) = delete;
    ~N64Controllers() { close(); }
    void close() {
        stop_rumble();
        for (auto& c : controllers) {
            if (c && SDL_WasInit(SDL_INIT_GAMECONTROLLER)) SDL_GameControllerClose(c);
            c = nullptr;
        }
    }
    void refresh() {
        for (size_t i = 0; i < controllers.size(); ++i) {
            auto& c = controllers[i];
            if (c && !SDL_GameControllerGetAttached(c)) {
                SDL_GameControllerClose(c);
                c = nullptr;
                rumbling[i] = false;
                blocked[i].fill(true);
            }
        }
        for (int i = 0; i < SDL_NumJoysticks(); ++i) {
            if (!SDL_IsGameController(i)) continue;
            const auto id = SDL_JoystickGetDeviceInstanceID(i);
            if (std::any_of(controllers.begin(), controllers.end(), [id](auto* c) {
                return c && SDL_JoystickInstanceID(SDL_GameControllerGetJoystick(c)) == id;
            })) continue;
            for (auto& c : controllers) if (!c) { c = SDL_GameControllerOpen(i); break; }
        }
    }
    SDL_JoystickID device_id(size_t player) const {
        return player < controllers.size() && controllers[player]
            ? SDL_JoystickInstanceID(SDL_GameControllerGetJoystick(controllers[player])) : -1;
    }
    const char* name(size_t player) const {
        const char* value = player < controllers.size() && controllers[player]
            ? SDL_GameControllerName(controllers[player]) : nullptr;
        return value ? value : "NO GAMEPAD";
    }
    bool has_rumble(size_t player) const {
        return player < controllers.size() && controllers[player] &&
            SDL_GameControllerHasRumble(controllers[player]) == SDL_TRUE;
    }
    void key_event(const SDL_Event& event) {
        if (event.type != SDL_KEYDOWN && event.type != SDL_KEYUP) return;
        if (event.type == SDL_KEYDOWN && event.key.repeat) return;
        for (size_t i = 0; i < keyboard.size(); ++i)
            if (event.key.keysym.sym == settings.keys[i]) keyboard[i] = event.type == SDL_KEYDOWN;
    }
    void release_inputs() {
        keyboard.fill(false);
        // Held pad controls must return to neutral after a menu or focus change.
        for (auto& port : blocked) port.fill(true);
        stop_rumble();
    }
    static std::int8_t axis(Sint16 raw, int deadzone = 6000) {
        const int value = raw;
        deadzone = std::clamp(deadzone, 0, 30000);
        if (std::abs(value) <= deadzone) return 0;
        const int magnitude = std::min(80, (std::abs(value) - deadzone) * 80 / (32767 - deadzone));
        return static_cast<std::int8_t>(value < 0 ? -magnitude : magnitude);
    }
    static int binding_value(N64Binding binding, const N64PadState& pad, int deadzone, bool analog) {
        if (!valid_n64_binding(binding)) return 0;
        if (binding.kind == N64BindingKind::Button) return pad.buttons[binding.index] ? 80 : 0;
        const int raw = pad.axes[binding.index];
        const int signed_value = binding.kind == N64BindingKind::AxisNegative ? -raw : raw;
        if (signed_value <= 0) return 0;
        if (!analog) return signed_value > std::max(16000, deadzone) ? 80 : 0;
        return std::abs(static_cast<int>(axis(pad.axes[binding.index], deadzone)));
    }
    static ffi::N64Input compose(const std::array<int, N64_CONTROLS.size()>& values,
                                bool invert_x, bool invert_y) {
        ffi::N64Input result{};
        int x = 0, y = 0;
        for (size_t i = 0; i < values.size(); ++i) {
            const int value = std::clamp(values[i], 0, 80);
            if (value) result.buttons |= N64_CONTROLS[i].mask;
            x += N64_CONTROLS[i].x * value;
            y += N64_CONTROLS[i].y * value;
        }
        result.stick_x = static_cast<std::int8_t>(std::clamp(invert_x ? -x : x, -80, 80));
        result.stick_y = static_cast<std::int8_t>(std::clamp(invert_y ? -y : y, -80, 80));
        return result;
    }
    ffi::N64Input read(size_t player, bool focused, ffi::N64Input scripted = {}) {
        ffi::N64Input result{};
        if (player >= controllers.size()) return result;
        const auto& port = settings.ports[player];
        result.connected = port.enabled && (player == 0 || controllers[player]);
        if (!focused || !result.connected) return result;
        N64PadState pad;
        if (auto* c = controllers[player]) {
            for (int i = 0; i < SDL_CONTROLLER_BUTTON_MAX; ++i)
                pad.buttons[i] = SDL_GameControllerGetButton(c, static_cast<SDL_GameControllerButton>(i)) != 0;
            for (int i = 0; i < SDL_CONTROLLER_AXIS_MAX; ++i)
                pad.axes[i] = SDL_GameControllerGetAxis(c, static_cast<SDL_GameControllerAxis>(i));
        }
        std::array<int, N64_CONTROLS.size()> values{};
        for (size_t i = 0; i < values.size(); ++i) {
            int value = binding_value(port.bindings[i], pad, port.deadzone, N64_CONTROLS[i].mask == 0);
            if (!value) blocked[player][i] = false;
            if (blocked[player][i]) value = 0;
            values[i] = player == 0 && keyboard[i] ? 80 : value;
        }
        const bool connected = result.connected;
        result = compose(values, port.invert_x, port.invert_y);
        result.connected = connected;
        if (player == 0) {
            result.buttons |= scripted.buttons;
            if (scripted.stick_x) result.stick_x = scripted.stick_x;
            if (scripted.stick_y) result.stick_y = scripted.stick_y;
        }
        return result;
    }
    std::optional<N64Binding> capture(const SDL_Event& event, size_t player) const {
        const auto id = device_id(player);
        if (id < 0) return std::nullopt;
        if (event.type == SDL_CONTROLLERBUTTONDOWN && event.cbutton.which == id)
            return N64Binding{N64BindingKind::Button, event.cbutton.button};
        if (event.type == SDL_CONTROLLERAXISMOTION && event.caxis.which == id && std::abs(int(event.caxis.value)) > 20000)
            return N64Binding{event.caxis.value < 0 ? N64BindingKind::AxisNegative : N64BindingKind::AxisPositive,
                              event.caxis.axis};
        return std::nullopt;
    }
    void set_rumble(size_t player, bool enabled) {
        if (player >= controllers.size()) return;
        enabled = enabled && settings.ports[player].enabled && settings.ports[player].rumble;
        const Uint32 now = SDL_GetTicks();
        if (enabled == rumbling[player] && (!enabled || now - rumble_at[player] < 150)) return;
        // Renew short pulses: if a guest stalls, vibration still expires on time.
        if (has_rumble(player)) SDL_GameControllerRumble(controllers[player], enabled ? 0xffff : 0,
                                                        enabled ? 0xffff : 0, enabled ? 300 : 0);
        rumbling[player] = enabled;
        rumble_at[player] = now;
    }
    void stop_rumble() {
        if (!SDL_WasInit(SDL_INIT_GAMECONTROLLER)) { rumbling.fill(false); return; }
        for (size_t i = 0; i < controllers.size(); ++i) set_rumble(i, false);
    }
};
