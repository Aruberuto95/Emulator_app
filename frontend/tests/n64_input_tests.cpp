#define SDL_MAIN_HANDLED
#include "n64_input.h"
#include "settings_menu.h"
#include <cstdio>
#include <fstream>

static int failures = 0;
static void check(bool value, const char* name) {
    if (!value) { std::fprintf(stderr, "%s: %s\n", name, SDL_GetError()); ++failures; }
}
struct RumbleProbe { bool active = false; int calls = 0; };
static int SDLCALL rumble_callback(void* data, Uint16 low, Uint16 high) {
    auto& probe = *static_cast<RumbleProbe*>(data);
    probe.active = low != 0 || high != 0;
    ++probe.calls;
    return 0;
}
int main() {
    SDL_SetMainReady();
    SDL_SetHint(SDL_HINT_JOYSTICK_ALLOW_BACKGROUND_EVENTS, "1");
    if (SDL_Init(SDL_INIT_GAMECONTROLLER) != 0) return 1;
    check(N64Controllers::axis(-32768) == -80, "negative endpoint");
    check(N64Controllers::axis(32767) == 80, "positive endpoint");
    for (int i = -6000; i <= 6000; ++i) check(N64Controllers::axis(static_cast<Sint16>(i)) == 0, "dead zone");
    for (int i = 6001; i < 32768; ++i)
        check(N64Controllers::axis(static_cast<Sint16>(i)) == -N64Controllers::axis(static_cast<Sint16>(-i)), "axis symmetry");
    for (int deadzone : {0, 6000, 30000}) {
        check(N64Controllers::axis(static_cast<Sint16>(deadzone), deadzone) == 0, "configured dead zone");
        check(N64Controllers::axis(-32768, deadzone) == -80, "configured negative endpoint");
    }
    N64Settings profile;
    check(!n64_settings_error(profile), "default profile valid");
    InputMapping legacy;
    legacy.a = SDLK_j;
    const auto migrated = n64_settings_from_legacy(legacy);
    check(!n64_settings_error(migrated) && migrated.keys[4] == SDLK_j && migrated.keys[12] != SDLK_j,
          "custom legacy key survives collision with new default");
    auto invalid = profile;
    invalid.keys[0] = SDLK_ESCAPE;
    check(n64_settings_error(invalid), "reserved key rejected");
    invalid = profile; invalid.keys[0] = invalid.keys[1];
    check(n64_settings_error(invalid), "duplicate key rejected");
    invalid = profile; invalid.ports[3].deadzone = 32767;
    check(n64_settings_error(invalid), "unsafe dead zone rejected");
    invalid = profile; invalid.ports[2].bindings[0].index = SDL_CONTROLLER_AXIS_MAX;
    check(n64_settings_error(invalid), "axis index bounded");
    invalid = profile; invalid.ports[1].bindings[0] = invalid.ports[1].bindings[1];
    check(n64_settings_error(invalid), "duplicate axis rejected");
    invalid = profile; invalid.ports[1].accessory = static_cast<ffi::N64Accessory>(255);
    check(n64_settings_error(invalid), "unknown accessory rejected");

    const auto rows = settings_rows(true);
    for (auto kind : {SettingKind::N64Key, SettingKind::N64Pad})
        check(std::count_if(rows.begin(), rows.end(), [kind](const auto& row) { return row.kind == kind; }) == 18,
              "all N64 controls shown");
    check(settings_rows(false).size() == 18, "other console menu retained");
    check(SettingsViewport(1152, 0, 18, 0).visible == 18, "large DS menu page cannot exceed row count");
    for (int height : {432, 720, 1152}) {
        int first = 0;
        for (int selected = 0; selected < static_cast<int>(rows.size()); ++selected) {
            SettingsViewport view(height, selected, static_cast<int>(rows.size()), first);
            first = view.first;
            check(selected >= view.first && selected < view.first + view.visible, "selection visible");
            check(view.top + (selected - view.first) * view.row_height + 8 < view.footer, "selection above footer");
            check(view.footer + 56 + 8 <= height, "footer inside viewport");
        }
        SettingsViewport wrapped(height, 0, static_cast<int>(rows.size()), first);
        check(wrapped.first == 0, "wrapped selection visible");
    }
    const auto directory = std::filesystem::temp_directory_path() / ("n64-profile-test-" + std::to_string(std::random_device{}()));
    std::filesystem::create_directory(directory);
    const auto path = directory / "profile.cfg";
    profile.keys[12] = SDLK_t;
    profile.ports[3].accessory = ffi::N64Accessory::RumblePak;
    profile.ports[2].deadzone = 10000;
    check(save_n64_settings(path, profile).empty(), "profile saved");
    N64Settings loaded;
    check(load_n64_settings(path, loaded).empty(), "profile loaded");
    check(loaded.keys == profile.keys && loaded.ports[2].deadzone == 10000 &&
          loaded.ports[3].accessory == ffi::N64Accessory::RumblePak, "complete profile round trip");
    loaded.ports[0].invert_x = true;
    check(save_n64_settings(path, loaded).empty(), "atomic replacement of existing file");
    check(load_n64_settings(path, profile).empty() && profile.ports[0].invert_x, "replacement persisted");
    const auto before = profile.keys;
    std::ofstream(path, std::ios::app) << "corruption";
    check(!load_n64_settings(path, profile).empty() && profile.keys == before, "corrupt file leaves profile intact");
    check(!save_n64_settings(directory / "missing" / "profile.cfg", profile).empty(), "write failure reported");
    std::ofstream(path) << std::string(8193, 'x');
    check(!load_n64_settings(path, profile).empty(), "oversized profile rejected");
    std::ofstream(path) << "N64_PROFILE 1\n1 2";
    check(!load_n64_settings(path, profile).empty() && profile.keys == before, "partial profile rejected atomically");
    std::filesystem::remove(path);
    std::filesystem::remove(directory);

    profile = N64Settings{};
    N64Controllers controls(profile);
    SDL_Event key{};
    key.type = SDL_KEYDOWN; key.key.keysym.sym = SDLK_j;
    controls.key_event(key);
    check(controls.read(0, true).buttons == 2, "C left received through configured keyboard");
    key.key.keysym.sym = SDLK_l; controls.key_event(key);
    check(controls.read(0, true).buttons == 3, "C directions distinct");
    controls.release_inputs();
    check(!controls.read(0, true).buttons, "menu releases keyboard");
    profile.keys[12] = SDLK_t;
    key.key.keysym.sym = SDLK_j; controls.key_event(key);
    check(!controls.read(0, true).buttons, "old binding removed");
    key.key.keysym.sym = SDLK_t; controls.key_event(key);
    check(controls.read(0, true).buttons == 2, "new binding applied immediately");
    controls.release_inputs();
    key.key.keysym.sym = SDLK_UP; controls.key_event(key);
    key.key.keysym.sym = SDLK_DOWN; controls.key_event(key);
    check(controls.read(0, true).stick_y == 0, "opposite stick directions cancel");
    key.type = SDL_KEYUP; controls.key_event(key);
    check(controls.read(0, true).stick_y == 80, "releasing opposite preserves held direction");
    profile.ports[0].invert_y = true;
    check(controls.read(0, true).stick_y == -80, "stick inversion");
    controls.release_inputs();
    ffi::N64Input held{}; held.buttons = 0xffff; held.stick_x = 80;
    const auto neutral = controls.read(0, false, held);
    check(neutral.connected && !neutral.buttons && !neutral.stick_x && !neutral.stick_y, "focus loss releases scripted input");
    check(!controls.read(4, true).connected, "invalid player disconnected");
    profile.ports[0].enabled = false;
    check(!controls.read(0, true, held).connected && !controls.read(0, true, held).buttons, "disabled port cannot receive input");
    profile = N64Settings{};

    std::array<SDL_Joystick*, 4> pads{};
    std::array<RumbleProbe, 4> rumble{};
    for (size_t i = 0; i < pads.size(); ++i) {
        SDL_VirtualJoystickDesc description{};
        description.version = SDL_VIRTUAL_JOYSTICK_DESC_VERSION;
        description.type = SDL_JOYSTICK_TYPE_GAMECONTROLLER;
        description.naxes = SDL_CONTROLLER_AXIS_MAX;
        description.nbuttons = SDL_CONTROLLER_BUTTON_MAX;
        description.axis_mask = (1u << SDL_CONTROLLER_AXIS_MAX) - 1;
        description.button_mask = (1u << SDL_CONTROLLER_BUTTON_MAX) - 1;
        description.name = "N64 integration test controller";
        description.userdata = &rumble[i]; description.Rumble = rumble_callback;
        const int index = SDL_JoystickAttachVirtualEx(&description);
        check(index >= 0, "virtual controller attached");
        if (index < 0) continue;
        pads[i] = SDL_JoystickOpen(index);
        SDL_JoystickSetVirtualAxis(pads[i], SDL_CONTROLLER_AXIS_TRIGGERLEFT, -32768);
        SDL_JoystickSetVirtualAxis(pads[i], SDL_CONTROLLER_AXIS_TRIGGERRIGHT, -32768);
    }
    controls.refresh();
    SDL_JoystickUpdate();
    for (size_t i = 0; i < pads.size(); ++i) {
        check(controls.device_id(i) >= 0 && controls.read(i, true).connected, "four ports connected");
        controls.read(i, true); // release any pre-existing neutral latch
        if (!pads[i]) continue;
        SDL_JoystickSetVirtualButton(pads[i], SDL_CONTROLLER_BUTTON_A, 1);
        SDL_JoystickUpdate();
        check(controls.read(i, true).buttons == 0x8000, "physical A reaches correct port");
        controls.release_inputs();
        check(!controls.read(i, true).buttons, "held pad suppressed on resume");
        SDL_JoystickSetVirtualButton(pads[i], SDL_CONTROLLER_BUTTON_A, 0); SDL_JoystickUpdate(); controls.read(i, true);
        SDL_JoystickSetVirtualButton(pads[i], SDL_CONTROLLER_BUTTON_A, 1); SDL_JoystickUpdate();
        check(controls.read(i, true).buttons == 0x8000, "pad resumes after neutral");
        SDL_JoystickSetVirtualButton(pads[i], SDL_CONTROLLER_BUTTON_A, 0); SDL_JoystickUpdate();
        controls.set_rumble(i, true);
        check(rumble[i].active, "physical vibration delivered");
        controls.stop_rumble();
        check(!rumble[i].active, "vibration stopped on pause");
    }
    SDL_Event binding{};
    binding.type = SDL_CONTROLLERAXISMOTION;
    binding.caxis.which = controls.device_id(2); binding.caxis.axis = SDL_CONTROLLER_AXIS_LEFTX; binding.caxis.value = -32768;
    check(controls.capture(binding, 2).has_value() && !controls.capture(binding, 1), "capture belongs to selected port");
    if (pads[1]) {
        SDL_JoystickClose(pads[1]); pads[1] = nullptr;
        check(SDL_JoystickDetachVirtual(1) == 0, "controller detached");
        controls.refresh();
        check(!controls.read(1, true).connected && controls.read(2, true).connected, "disconnect preserves other ports");
    }
    controls.close();
    for (auto* pad : pads) if (pad) SDL_JoystickClose(pad);
    SDL_Quit();
    if (!failures) std::puts("N64 controls, profiles, menu, four ports and rumble passed");
    return failures ? 1 : 0;
}
