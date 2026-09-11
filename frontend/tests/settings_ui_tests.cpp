// Exercise the actual application loop, event dispatch and SDL drawing, without
// adding production test hooks or relying on desktop focus/input injection.
#define SDL_MAIN_HANDLED
#include <SDL.h>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <filesystem>

static int ui_failures = 0;
static int ui_frame = 0;
static std::string capture_directory;
static void checked_present(SDL_Renderer* renderer);
#define SDL_RenderPresent checked_present
#define main emulator_application_main
#include "../src/main.cpp"
#undef main
#undef SDL_RenderPresent

static void send_key(SDL_Keycode key) {
    SDL_Event event{};
    event.type = SDL_KEYDOWN;
    event.key.keysym.sym = key;
    SDL_PushEvent(&event);
    event.type = SDL_KEYUP;
    SDL_PushEvent(&event);
}
static void checked_present(SDL_Renderer* renderer) {
    if (ui_frame > 0 && ui_frame <= 49) {
        int width = 0, height = 0;
        SDL_GetRendererOutputSize(renderer, &width, &height);
        auto* frame = SDL_CreateRGBSurfaceWithFormat(0, width, height, 32, SDL_PIXELFORMAT_RGBA32);
        bool selected_visible = false;
        if (frame && SDL_RenderReadPixels(renderer, nullptr, frame->format->format, frame->pixels, frame->pitch) == 0) {
            // Only the selected row uses pure green. It must survive clipping
            // for every control/action, including the last row after scrolling.
            for (int y = 0; y < height; ++y) {
                const auto* pixels = static_cast<const Uint8*>(frame->pixels) + y * frame->pitch;
                for (int x = 0; x < width; ++x) {
                    const auto* p = pixels + x * 4;
                    if (p[0] == 0 && p[1] == 255 && p[2] == 0) selected_visible = true;
                }
            }
            if (!capture_directory.empty() && (ui_frame == 1 || ui_frame == 25 || ui_frame == 43 || ui_frame == 44 || ui_frame == 49)) {
                const auto path = std::filesystem::path(capture_directory) / ("menu-row-" + std::to_string(ui_frame) + ".bmp");
                if (SDL_SaveBMP(frame, path.string().c_str()) != 0) ++ui_failures;
            }
        }
        if (frame) SDL_FreeSurface(frame);
        if (!selected_visible) { std::fprintf(stderr, "Settings row %d is not visible\n", ui_frame); ++ui_failures; }
    }
    SDL_RenderPresent(renderer);
    if (ui_frame == 0) send_key(SDLK_ESCAPE);
    else if (ui_frame < 49) send_key(SDLK_DOWN);
    else {
        SDL_Event quit{};
        quit.type = SDL_QUIT;
        SDL_PushEvent(&quit);
    }
    ++ui_frame;
}
int main() {
    const char* rom = std::getenv("N64_TEST_ROM");
    if (!rom) { std::puts("SKIP: settings UI requires N64_TEST_ROM (use a temporary ROM copy)"); return 0; }
    if (const char* directory = std::getenv("N64_UI_OUTPUT_DIR")) capture_directory = directory;
    SDL_setenv("SDL_VIDEODRIVER", "dummy", 1);
    SDL_setenv("SDL_AUDIODRIVER", "dummy", 1);
    SDL_setenv("SDL_RENDER_VSYNC", "0", 1);
    char app[] = "settings_ui_tests", rom_option[] = "--rom", pause_option[] = "--pause";
    std::string rom_path = rom;
    char* arguments[] = {app, rom_option, rom_path.data(), pause_option};
    const int result = emulator_application_main(4, arguments);
    // The existing loop may present its final paused frame after SDL_QUIT.
    if (result != 0 || ui_frame < 50 || ui_frame > 51 || ui_failures) {
        std::fprintf(stderr, "UI test failed: exit=%d frames=%d failures=%d\n", result, ui_frame, ui_failures);
        return 1;
    }
    std::puts("Esc and all 49 N64 settings rows rendered successfully");
    return 0;
}
