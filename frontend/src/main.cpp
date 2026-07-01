#include "rust/cxx.h"
#include "core/src/lib.rs.h"
#include <iostream>
#include <string>
#include <vector>
#include <map>
#include <fstream>
#include <sstream>
#include <cctype>
#include <algorithm>
#include <sys/stat.h>
#include <cstdlib>
#include <cmath>
#include <cstdio>
#define SDL_MAIN_HANDLED
#include <SDL.h>
#include <filesystem>
#include <ctime>
#include <chrono>

// Stable, writable per-user directory for config + savestates
// (e.g. %APPDATA%/EmulatorApp/ on Windows). This decouples persistence from the
// process working directory, which is unreliable: the executable is launched from
// build/bin while assets live in the project root. Falls back to the executable
// directory, then "./". The returned path always ends with a path separator.
static std::string config_dir() {
    static std::string cached;
    if (!cached.empty()) {
        return cached;
    }
    if (char* pref = SDL_GetPrefPath("EmulatorApp", "EmulatorApp")) {
        cached = pref;
        SDL_free(pref);
    }
    if (cached.empty()) {
        if (char* base = SDL_GetBasePath()) {
            cached = base;
            SDL_free(base);
        }
    }
    if (cached.empty()) {
        cached = "./";
    }
    std::error_code ec;
    std::filesystem::create_directories(cached, ec);
    return cached;
}

static const unsigned char font8x8_basic[128][8] = {
    // 0-31: control chars (empty)
    {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0},
    {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0}, {0},
    // 32: space
    {0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00},
    // 33: !
    {0x18, 0x18, 0x18, 0x18, 0x18, 0x00, 0x18, 0x00},
    // 34: "
    {0x66, 0x66, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00},
    // 35: #
    {0x36, 0x36, 0x7f, 0x36, 0x7f, 0x36, 0x36, 0x00},
    // 36: $
    {0x18, 0x3e, 0x60, 0x3c, 0x06, 0x7c, 0x18, 0x00},
    // 37: %
    {0x00, 0x66, 0x6c, 0x18, 0x30, 0x66, 0x46, 0x00},
    // 38: &
    {0x38, 0x6c, 0x38, 0x76, 0x6c, 0x6c, 0x3a, 0x00},
    // 39: '
    {0x18, 0x18, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00},
    // 40: (
    {0x0c, 0x18, 0x30, 0x30, 0x30, 0x18, 0x0c, 0x00},
    // 41: )
    {0x30, 0x18, 0x0c, 0x0c, 0x0c, 0x18, 0x30, 0x00},
    // 42: *
    {0x00, 0x66, 0x3c, 0xff, 0x3c, 0x66, 0x00, 0x00},
    // 43: +
    {0x00, 0x18, 0x18, 0x7e, 0x18, 0x18, 0x00, 0x00},
    // 44: ,
    {0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x30},
    // 45: -
    {0x00, 0x00, 0x00, 0x7e, 0x00, 0x00, 0x00, 0x00},
    // 46: .
    {0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00},
    // 47: /
    {0x00, 0x06, 0x0c, 0x18, 0x30, 0x60, 0x40, 0x00},
    // 48: 0
    {0x3c, 0x66, 0x6e, 0x76, 0x66, 0x66, 0x3c, 0x00},
    // 49: 1
    {0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x3c, 0x00},
    // 50: 2
    {0x3c, 0x66, 0x06, 0x0c, 0x30, 0x60, 0x7f, 0x00},
    // 51: 3
    {0x3c, 0x66, 0x06, 0x1c, 0x06, 0x66, 0x3c, 0x00},
    // 52: 4
    {0x0c, 0x1c, 0x3c, 0x6c, 0x7f, 0x0c, 0x0c, 0x00},
    // 53: 5
    {0x7f, 0x60, 0x7c, 0x06, 0x06, 0x66, 0x3c, 0x00},
    // 54: 6
    {0x3c, 0x66, 0x60, 0x7c, 0x66, 0x66, 0x3c, 0x00},
    // 55: 7
    {0x7f, 0x66, 0x0c, 0x18, 0x18, 0x18, 0x18, 0x00},
    // 56: 8
    {0x3c, 0x66, 0x66, 0x3c, 0x66, 0x66, 0x3c, 0x00},
    // 57: 9
    {0x3c, 0x66, 0x66, 0x3e, 0x06, 0x66, 0x3c, 0x00},
    // 58: :
    {0x00, 0x18, 0x18, 0x00, 0x00, 0x18, 0x18, 0x00},
    // 59: ;
    {0x00, 0x18, 0x18, 0x00, 0x00, 0x18, 0x18, 0x30},
    // 60: <
    {0x0c, 0x18, 0x30, 0x60, 0x30, 0x18, 0x0c, 0x00},
    // 61: =
    {0x00, 0x00, 0x7e, 0x00, 0x7e, 0x00, 0x00, 0x00},
    // 62: >
    {0x30, 0x18, 0x0c, 0x06, 0x0c, 0x18, 0x30, 0x00},
    // 63: ?
    {0x3c, 0x66, 0x06, 0x0c, 0x18, 0x00, 0x18, 0x00},
    // 64: @
    {0x3c, 0x66, 0x6e, 0x6a, 0x6e, 0x60, 0x3c, 0x00},
    // 65: A
    {0x18, 0x3c, 0x66, 0x7e, 0x66, 0x66, 0x66, 0x00},
    // 66: B
    {0x7c, 0x66, 0x66, 0x7c, 0x66, 0x66, 0x7c, 0x00},
    // 67: C
    {0x3c, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3c, 0x00},
    // 68: D
    {0x78, 0x6c, 0x66, 0x66, 0x66, 0x6c, 0x78, 0x00},
    // 69: E
    {0x7f, 0x60, 0x60, 0x7c, 0x60, 0x60, 0x7f, 0x00},
    // 70: F
    {0x7f, 0x60, 0x60, 0x7c, 0x60, 0x60, 0x60, 0x00},
    // 71: G
    {0x3c, 0x66, 0x60, 0x6e, 0x66, 0x66, 0x3c, 0x00},
    // 72: H
    {0x66, 0x66, 0x66, 0x7e, 0x66, 0x66, 0x66, 0x00},
    // 73: I
    {0x3c, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3c, 0x00},
    // 74: J
    {0x1e, 0x0c, 0x0c, 0x0c, 0x0c, 0xcc, 0x78, 0x00},
    // 75: K
    {0x66, 0x6c, 0x78, 0x70, 0x78, 0x6c, 0x66, 0x00},
    // 76: L
    {0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7f, 0x00},
    // 77: M
    {0x63, 0x77, 0x7f, 0x6b, 0x63, 0x63, 0x63, 0x00},
    // 78: N
    {0x66, 0x66, 0x76, 0x7e, 0x6e, 0x66, 0x66, 0x00},
    // 79: O
    {0x3c, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3c, 0x00},
    // 80: P
    {0x7c, 0x66, 0x66, 0x7c, 0x60, 0x60, 0x60, 0x00},
    // 81: Q
    {0x3c, 0x66, 0x66, 0x66, 0x6e, 0x7c, 0x0e, 0x00},
    // 82: R
    {0x7c, 0x66, 0x66, 0x7c, 0x6c, 0x66, 0x66, 0x00},
    // 83: S
    {0x3c, 0x66, 0x30, 0x1c, 0x06, 0x66, 0x3c, 0x00},
    // 84: T
    {0x7f, 0x5a, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00},
    // 85: U
    {0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3c, 0x00},
    // 86: V
    {0x66, 0x66, 0x66, 0x66, 0x66, 0x3c, 0x18, 0x00},
    // 87: W
    {0x63, 0x63, 0x63, 0x6b, 0x7f, 0x77, 0x63, 0x00},
    // 88: X
    {0x66, 0x66, 0x3c, 0x18, 0x3c, 0x66, 0x66, 0x00},
    // 89: Y
    {0x66, 0x66, 0x66, 0x3c, 0x18, 0x18, 0x18, 0x00},
    // 90: Z
    {0x7f, 0x06, 0x0c, 0x18, 0x30, 0x60, 0x7f, 0x00},
    // 91: [
    {0x3c, 0x30, 0x30, 0x30, 0x30, 0x30, 0x3c, 0x00},
    // 92: backslash (do NOT end this comment with a literal '\' - it line-continues
    // into the next line and silently drops this glyph, shifting the whole table)
    {0x00, 0x40, 0x30, 0x18, 0x0c, 0x06, 0x02, 0x00},
    // 93: ]
    {0x3c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x3c, 0x00},
    // 94: ^
    {0x18, 0x3c, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00},
    // 95: _
    {0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x00},
    // 96: `
    {0x30, 0x18, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00},
    // 97: a
    {0x00, 0x00, 0x3c, 0x06, 0x3e, 0x66, 0x3e, 0x00},
    // 98: b
    {0x60, 0x60, 0x7c, 0x66, 0x66, 0x66, 0x7c, 0x00},
    // 99: c
    {0x00, 0x00, 0x3c, 0x60, 0x60, 0x66, 0x3c, 0x00},
    // 100: d
    {0x06, 0x06, 0x3e, 0x66, 0x66, 0x66, 0x3e, 0x00},
    // 101: e
    {0x00, 0x00, 0x3c, 0x66, 0x7e, 0x60, 0x3c, 0x00},
    // 102: f
    {0x1c, 0x30, 0x78, 0x30, 0x30, 0x30, 0x30, 0x00},
    // 103: g
    {0x00, 0x00, 0x3e, 0x66, 0x66, 0x3e, 0x06, 0x7c},
    // 104: h
    {0x60, 0x60, 0x7c, 0x66, 0x66, 0x66, 0x66, 0x00},
    // 105: i
    {0x18, 0x00, 0x38, 0x18, 0x18, 0x18, 0x3c, 0x00},
    // 106: j
    {0x0c, 0x00, 0x1c, 0x0c, 0x0c, 0x0c, 0x0c, 0x78},
    // 107: k
    {0x60, 0x60, 0x66, 0x6c, 0x78, 0x6c, 0x66, 0x00},
    // 108: l
    {0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3c, 0x00},
    // 109: m
    {0x00, 0x00, 0x6c, 0xfe, 0xfe, 0xd6, 0xc6, 0x00},
    // 110: n
    {0x00, 0x00, 0x7c, 0x66, 0x66, 0x66, 0x66, 0x00},
    // 111: o
    {0x00, 0x00, 0x3c, 0x66, 0x66, 0x66, 0x3c, 0x00},
    // 112: p
    {0x00, 0x00, 0x7c, 0x66, 0x66, 0x7c, 0x60, 0x60},
    // 113: q
    {0x00, 0x00, 0x3e, 0x66, 0x66, 0x3e, 0x06, 0x06},
    // 114: r
    {0x00, 0x00, 0x7c, 0x66, 0x60, 0x60, 0x60, 0x00},
    // 115: s
    {0x00, 0x00, 0x3e, 0x60, 0x3c, 0x06, 0x7c, 0x00},
    // 116: t
    {0x30, 0x30, 0x7c, 0x30, 0x30, 0x34, 0x18, 0x00},
    // 117: u
    {0x00, 0x00, 0x66, 0x66, 0x66, 0x6c, 0x3b, 0x00},
    // 118: v
    {0x00, 0x00, 0x66, 0x66, 0x66, 0x3c, 0x18, 0x00},
    // 119: w
    {0x00, 0x00, 0xc6, 0xd6, 0xfe, 0xee, 0x66, 0x00},
    // 120: x
    {0x00, 0x00, 0x66, 0x3c, 0x18, 0x3c, 0x66, 0x00},
    // 121: y
    {0x00, 0x00, 0x66, 0x66, 0x66, 0x3e, 0x06, 0x3c},
    // 122: z
    {0x00, 0x00, 0x7e, 0x0c, 0x18, 0x30, 0x7e, 0x00},
    // 123: {
    {0x0e, 0x18, 0x18, 0x70, 0x18, 0x18, 0x0e, 0x00},
    // 124: |
    {0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00},
    // 125: }
    {0x70, 0x18, 0x18, 0x0e, 0x18, 0x18, 0x70, 0x00},
    // 126: ~
    {0x76, 0x89, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00},
    // 127: delta (filled block fallback)
    {0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff}
};

void draw_char(SDL_Renderer* renderer, char c, int x, int y, int scale, SDL_Color color) {
    unsigned char uc = static_cast<unsigned char>(c);
    if (uc > 127) uc = 127;
    const unsigned char* glyph = font8x8_basic[uc];
    SDL_SetRenderDrawColor(renderer, color.r, color.g, color.b, color.a);
    for (int row = 0; row < 8; ++row) {
        unsigned char row_byte = glyph[row];
        for (int col = 0; col < 8; ++col) {
            if ((row_byte & (1 << (7 - col))) != 0) {
                SDL_Rect r = { x + col * scale, y + row * scale, scale, scale };
                SDL_RenderFillRect(renderer, &r);
            }
        }
    }
}

void draw_text(SDL_Renderer* renderer, const std::string& text, int x, int y, int scale, SDL_Color color) {
    int current_x = x;
    for (char c : text) {
        draw_char(renderer, c, current_x, y, scale, color);
        current_x += 8 * scale;
    }
}

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
};

static InputMapping user_mappings;

void save_input_mappings() {
    std::ofstream f(config_dir() + "input_mappings.json");
    if (f.is_open()) {
        f << "{\n";
        f << "  \"UP\": " << user_mappings.up << ",\n";
        f << "  \"DOWN\": " << user_mappings.down << ",\n";
        f << "  \"LEFT\": " << user_mappings.left << ",\n";
        f << "  \"RIGHT\": " << user_mappings.right << ",\n";
        f << "  \"A\": " << user_mappings.a << ",\n";
        f << "  \"B\": " << user_mappings.b << ",\n";
        f << "  \"L\": " << user_mappings.l << ",\n";
        f << "  \"R\": " << user_mappings.r << ",\n";
        f << "  \"START\": " << user_mappings.start << ",\n";
        f << "  \"SELECT\": " << user_mappings.select << "\n";
        f << "}\n";
        f.close();
    }
}

void load_input_mappings() {
    std::ifstream f(config_dir() + "input_mappings.json");
    if (!f.is_open()) {
        return;
    }
    std::string line;
    while (std::getline(f, line)) {
        size_t colon = line.find(':');
        if (colon == std::string::npos) continue;
        std::string key = line.substr(0, colon);
        std::string val_str = line.substr(colon + 1);
        key.erase(remove_if(key.begin(), key.end(), [](unsigned char c) { return isspace(c) || c == '"'; }), key.end());
        val_str.erase(remove_if(val_str.begin(), val_str.end(), [](unsigned char c) { return isspace(c) || c == ',' || c == '}'; }), val_str.end());
        if (val_str.empty()) continue;
        try {
            int val = std::stoi(val_str);
            if (key == "UP") user_mappings.up = val;
            else if (key == "DOWN") user_mappings.down = val;
            else if (key == "LEFT") user_mappings.left = val;
            else if (key == "RIGHT") user_mappings.right = val;
            else if (key == "A") user_mappings.a = val;
            else if (key == "B") user_mappings.b = val;
            else if (key == "L") user_mappings.l = val;
            else if (key == "R") user_mappings.r = val;
            else if (key == "START") user_mappings.start = val;
            else if (key == "SELECT") user_mappings.select = val;
        } catch (...) {}
    }
    f.close();

    // Reject a corrupt config: any zero keycode or a key bound to two actions
    // would leave actions unreachable (e.g. arrows doubling as A/B). Fall back to
    // defaults so input is always playable.
    const SDL_Keycode codes[] = {
        user_mappings.up, user_mappings.down, user_mappings.left, user_mappings.right,
        user_mappings.a, user_mappings.b, user_mappings.l, user_mappings.r,
        user_mappings.start, user_mappings.select,
    };
    const size_t n = sizeof(codes) / sizeof(codes[0]);
    for (size_t i = 0; i < n; ++i) {
        if (codes[i] == 0) { user_mappings = InputMapping{}; return; }
        for (size_t j = i + 1; j < n; ++j) {
            if (codes[i] == codes[j]) { user_mappings = InputMapping{}; return; }
        }
    }
}

struct RomEntry {
    std::string path;
    std::string console_type;
};

std::vector<RomEntry> parse_scanned_roms(const std::string& json_str) {
    std::vector<RomEntry> roms;
    size_t pos = 0;
    while (true) {
        size_t path_pos = json_str.find("\"path\":\"", pos);
        if (path_pos == std::string::npos) break;
        path_pos += 8;
        size_t path_end = json_str.find("\"", path_pos);
        if (path_end == std::string::npos) break;
        std::string path = json_str.substr(path_pos, path_end - path_pos);

        size_t bs = 0;
        while ((bs = path.find("\\\\", bs)) != std::string::npos) {
            path.replace(bs, 2, "\\");
            bs += 1;
        }

        size_t console_pos = json_str.find("\"console_type\":\"", path_end);
        if (console_pos == std::string::npos) break;
        console_pos += 16;
        size_t console_end = json_str.find("\"", console_pos);
        if (console_end == std::string::npos) break;
        std::string console_type = json_str.substr(console_pos, console_end - console_pos);

        roms.push_back({path, console_type});
        pos = console_end;
    }
    return roms;
}

static bool is_rom_file(const std::filesystem::path& p) {
    std::string ext = p.extension().string();
    std::transform(ext.begin(), ext.end(), ext.begin(), [](unsigned char c) { return std::tolower(c); });
    return ext == ".gb" || ext == ".gbc" || ext == ".gba";
}

// Lists a directory for the in-app file browser: a ".." entry (unless at a filesystem
// root), then subdirectories, then ROM files, each group sorted by name. Reuses RomEntry,
// overloading `console_type` as the row kind: "UP", "DIR", or the console label ("GBC"/"GBA").
// `path` is the absolute target. Inaccessible entries are skipped, never thrown.
std::vector<RomEntry> list_browser_dir(const std::string& dir) {
    std::vector<RomEntry> entries;
    std::error_code ec;
    std::filesystem::path base(dir);

    std::filesystem::path parent = base.parent_path();
    if (!parent.empty() && parent != base) {
        entries.push_back({parent.string(), "UP"});
    }

    std::vector<RomEntry> dirs, roms;
    std::filesystem::directory_iterator it(base, std::filesystem::directory_options::skip_permission_denied, ec);
    std::filesystem::directory_iterator end;
    for (; !ec && it != end; it.increment(ec)) {
        std::error_code ec2;
        const std::filesystem::path& p = it->path();
        if (it->is_directory(ec2)) {
            dirs.push_back({p.string(), "DIR"});
        } else if (it->is_regular_file(ec2) && is_rom_file(p)) {
            std::string ext = p.extension().string();
            std::transform(ext.begin(), ext.end(), ext.begin(), [](unsigned char c) { return std::tolower(c); });
            roms.push_back({p.string(), ext == ".gba" ? "GBA" : "GBC"});
        }
    }

    auto by_name = [](const RomEntry& a, const RomEntry& b) {
        return std::filesystem::path(a.path).filename().string() <
               std::filesystem::path(b.path).filename().string();
    };
    std::sort(dirs.begin(), dirs.end(), by_name);
    std::sort(roms.begin(), roms.end(), by_name);
    entries.insert(entries.end(), dirs.begin(), dirs.end());
    entries.insert(entries.end(), roms.begin(), roms.end());
    return entries;
}

// Filesystem name of the savestate file the core writes for `rom_path` + `slot`,
// mirroring savestate.rs: "<rom_stem>_savestate_<slot>.sav". Used by the save menu to
// show which slots are occupied. Empty rom_path → core's no-ROM fallback name.
static std::string savestate_filename(const std::string& rom_path, int slot) {
    std::string stem = rom_path.empty() ? std::string() : std::filesystem::path(rom_path).stem().string();
    if (stem.empty()) {
        return "savestate_" + std::to_string(slot) + ".sav";
    }
    return stem + "_savestate_" + std::to_string(slot) + ".sav";
}

struct CliArgs {
    bool headless = false;
    bool test_mode = false;
    int ticks = 0;
    std::string input_inject = "";
    std::string dump_video = "";
    std::string dump_audio = "";
    std::string dump_state = "";
    bool play = false;
    bool pause = false;
    bool reset = false;
    bool interactive = false;
    std::string rom = "";
    float speed = 1.0f;
    bool has_speed = false;
    int frame_skip = 0;
    bool has_frame_skip = false;
};

bool parse_args(int argc, char* argv[], CliArgs& args) {
    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--headless") {
            args.headless = true;
        } else if (arg == "--test-mode") {
            args.test_mode = true;
        } else if (arg == "--play") {
            args.play = true;
        } else if (arg == "--pause") {
            args.pause = true;
        } else if (arg == "--reset") {
            args.reset = true;
        } else if (arg == "--interactive") {
            args.interactive = true;
        } else if (arg == "--rom") {
            if (i + 1 >= argc) {
                std::cerr << "Error: --rom requires an argument\n";
                return false;
            }
            args.rom = argv[++i];
        } else if (arg == "--speed") {
            if (i + 1 >= argc) {
                std::cerr << "Error: --speed requires an argument\n";
                return false;
            }
            try {
                float val = std::stof(argv[++i]);
                if (val <= 0.0f) {
                    std::cerr << "Error: Speed must be positive\n";
                    std::exit(1);
                }
                if (val > 1000.0f) {
                    std::cerr << "Error: Speed exceeds maximum limit\n";
                    std::exit(1);
                }
                args.speed = val;
                args.has_speed = true;
            } catch (...) {
                std::cerr << "Error: Non-numeric speed\n";
                std::exit(1);
            }
        } else if (arg == "--frame-skip") {
            if (i + 1 >= argc) {
                std::cerr << "Error: --frame-skip requires an argument\n";
                return false;
            }
            try {
                int val = std::stoi(argv[++i]);
                if (val < 0) {
                    std::cerr << "Error: Frame skip cannot be negative\n";
                    std::exit(1);
                }
                if (val > 1000) {
                    std::cerr << "Error: Frame skip exceeds maximum limit\n";
                    std::exit(1);
                }
                args.frame_skip = val;
                args.has_frame_skip = true;
            } catch (...) {
                std::cerr << "Error: Non-integer frame skip\n";
                std::exit(1);
            }
        } else if (arg == "--ticks") {
            if (i + 1 >= argc) {
                std::cerr << "Error: --ticks requires an argument\n";
                return false;
            }
            try {
                // Pre-check for huge tick values to avoid overflow
                std::string tick_str = argv[i+1];
                size_t first_non_zero = tick_str.find_first_not_of('0');
                std::string stripped = (first_non_zero == std::string::npos) ? "0" : tick_str.substr(first_non_zero);
                if (stripped.size() > 18) {
                    std::cerr << "Error: Invalid ticks value\n";
                    std::exit(1);
                }
                long long val = std::stoll(stripped);
                if (val < 0) {
                    std::cerr << "Error: --ticks cannot be negative\n";
                    std::exit(1);
                }
                args.ticks = static_cast<int>(val);
                i++;
            } catch (...) {
                std::cerr << "Error: Invalid ticks value\n";
                std::exit(1);
            }
        } else if (arg == "--input-inject") {
            if (i + 1 >= argc) {
                std::cerr << "Error: --input-inject requires an argument\n";
                return false;
            }
            args.input_inject = argv[++i];
        } else if (arg == "--dump-video") {
            if (i + 1 >= argc) {
                std::cerr << "Error: --dump-video requires an argument\n";
                return false;
            }
            args.dump_video = argv[++i];
        } else if (arg == "--dump-audio") {
            if (i + 1 >= argc) {
                std::cerr << "Error: --dump-audio requires an argument\n";
                return false;
            }
            args.dump_audio = argv[++i];
        } else if (arg == "--dump-state") {
            if (i + 1 >= argc) {
                std::cerr << "Error: --dump-state requires an argument\n";
                return false;
            }
            args.dump_state = argv[++i];
        } else {
            std::cerr << "Error: Unknown CLI arguments detected: " << arg << "\n";
            return false;
        }
    }
    return true;
}

bool file_exists(const std::string& path) {
    struct stat buffer;
    return (stat(path.c_str(), &buffer) == 0);
}

bool is_safe_path(const std::string& path) {
    if (path.find("..") != std::string::npos) {
        return false;
    }
    const char* allowed_dir_env = std::getenv("ALLOWED_DUMP_DIR");
    if (allowed_dir_env != nullptr) {
        std::filesystem::path allowed_path = std::filesystem::weakly_canonical(allowed_dir_env);
        std::string allowed_str = allowed_path.string();
        if (allowed_str.empty() || allowed_str.back() != '/') {
            allowed_str += '/';
        }
        std::filesystem::path p(path);
        if (p.is_absolute()) {
            std::string resolved_path = std::filesystem::weakly_canonical(p).string();
            if (resolved_path.rfind(allowed_str, 0) != 0) {
                return false;
            }
        }
    }
    return true;
}

std::string read_file(const std::string& path) {
    std::ifstream f(path, std::ios::binary);
    if (!f.is_open()) return "";

    const size_t max_size = 1048576; // 1MB
    std::vector<char> buffer(max_size + 1);
    f.read(buffer.data(), max_size + 1);
    std::streamsize bytes_read = f.gcount();

    if (bytes_read > static_cast<std::streamsize>(max_size)) {
        f.close();
        return "";
    }

    f.close();
    return std::string(buffer.data(), bytes_read);
}

std::string trim(const std::string& str) {
    size_t first = str.find_first_not_of(" \t\r\n");
    if (first == std::string::npos) return "";
    size_t last = str.find_last_not_of(" \t\r\n");
    return str.substr(first, last - first + 1);
}

bool get_bool_field(const std::string& json, const std::string& key) {
    size_t pos = json.find("\"" + key + "\"");
    if (pos == std::string::npos) return false;
    size_t colon = json.find(":", pos);
    if (colon == std::string::npos) return false;
    size_t next_char_pos = json.find_first_not_of(" \t\r\n", colon + 1);
    if (next_char_pos == std::string::npos) return false;
    if (json.compare(next_char_pos, 4, "true") == 0) {
        return true;
    }
    return false;
}

struct FrameInput {
    int frame;
    ffi::ButtonState buttons;
};

std::vector<FrameInput> parse_json_sequence(const std::string& json_str) {
    std::vector<FrameInput> sequence;
    int depth = 0;
    size_t start_pos = 0;
    for (size_t i = 0; i < json_str.size(); ++i) {
        if (json_str[i] == '{') {
            if (depth == 0) {
                start_pos = i;
            }
            depth++;
        } else if (json_str[i] == '}') {
            depth--;
            if (depth == 0 && start_pos < i) {
                std::string obj_str = json_str.substr(start_pos, i - start_pos + 1);
                int frame = 0;
                size_t frame_pos = obj_str.find("\"frame\"");
                if (frame_pos != std::string::npos) {
                    size_t colon = obj_str.find(":", frame_pos);
                    if (colon != std::string::npos) {
                        size_t next_char_pos = obj_str.find_first_not_of(" \t\r\n", colon + 1);
                        if (next_char_pos != std::string::npos) {
                            std::string num_str;
                            while (next_char_pos < obj_str.size() && std::isdigit(obj_str[next_char_pos])) {
                                num_str += obj_str[next_char_pos];
                                next_char_pos++;
                            }
                            if (!num_str.empty()) {
                                frame = std::stoi(num_str);
                            }
                        }
                    }
                }
                
                ffi::ButtonState bs = {false, false, false, false, false, false, false, false, false, false};
                size_t buttons_pos = obj_str.find("\"buttons\"");
                if (buttons_pos != std::string::npos) {
                    size_t start_brace = obj_str.find("{", buttons_pos);
                    size_t end_brace = obj_str.find("}", start_brace);
                    if (start_brace != std::string::npos && end_brace != std::string::npos) {
                        std::string buttons_str = obj_str.substr(start_brace, end_brace - start_brace + 1);
                        bs.up = get_bool_field(buttons_str, "up");
                        bs.down = get_bool_field(buttons_str, "down");
                        bs.left = get_bool_field(buttons_str, "left");
                        bs.right = get_bool_field(buttons_str, "right");
                        bs.a = get_bool_field(buttons_str, "a");
                        bs.b = get_bool_field(buttons_str, "b");
                        bs.start = get_bool_field(buttons_str, "start");
                        bs.select = get_bool_field(buttons_str, "select");
                        bs.l = get_bool_field(buttons_str, "l");
                        bs.r = get_bool_field(buttons_str, "r");
                    }
                }
                sequence.push_back({frame, bs});
            }
        }
    }
    return sequence;
}

ffi::ButtonState parse_single_button_state(const std::string& json_str) {
    ffi::ButtonState bs = {false, false, false, false, false, false, false, false, false, false};
    bs.up = get_bool_field(json_str, "up");
    bs.down = get_bool_field(json_str, "down");
    bs.left = get_bool_field(json_str, "left");
    bs.right = get_bool_field(json_str, "right");
    bs.a = get_bool_field(json_str, "a");
    bs.b = get_bool_field(json_str, "b");
    bs.start = get_bool_field(json_str, "start");
    bs.select = get_bool_field(json_str, "select");
    bs.l = get_bool_field(json_str, "l");
    bs.r = get_bool_field(json_str, "r");
    return bs;
}

bool dump_state_to_file(const rust::Box<ffi::Emulator>& emu, const std::string& path, bool is_playing_state) {
    std::ofstream outfile(path);
    if (!outfile.is_open()) return false;

    rust::Slice<const uint8_t> video = ffi::get_video_buffer(*emu);
    rust::Slice<const int16_t> audio = ffi::get_audio_buffer(*emu);

    uintptr_t video_addr = reinterpret_cast<uintptr_t>(video.data());
    uintptr_t audio_addr = reinterpret_cast<uintptr_t>(audio.data());

    ffi::ButtonState bs = ffi::get_button_state(*emu);
    bool is_gba = (ffi::get_console_type(*emu) == ffi::ConsoleType::Gba);

    outfile << "{\n"
            << "  \"playback_state\": \"" << (is_playing_state ? "play" : "pause") << "\",\n"
            << "  \"state\": \"" << std::string(ffi::get_state_string(*emu)) << "\",\n"
            << "  \"ticks\": " << ffi::get_ticks(*emu) << ",\n"
            << "  \"console_type\": \"" << (is_gba ? "GBA" : "GBC") << "\",\n"
            << "  \"player_x\": " << static_cast<int>(ffi::get_player_x(*emu)) << ",\n"
            << "  \"player_y\": " << static_cast<int>(ffi::get_player_y(*emu)) << ",\n"
            << "  \"buttons\": {\n"
            << "    \"up\": " << (bs.up ? "true" : "false") << ",\n"
            << "    \"down\": " << (bs.down ? "true" : "false") << ",\n"
            << "    \"left\": " << (bs.left ? "true" : "false") << ",\n"
            << "    \"right\": " << (bs.right ? "true" : "false") << ",\n"
            << "    \"a\": " << (bs.a ? "true" : "false") << ",\n"
            << "    \"b\": " << (bs.b ? "true" : "false") << ",\n"
            << "    \"start\": " << (bs.start ? "true" : "false") << ",\n"
            << "    \"select\": " << (bs.select ? "true" : "false") << ",\n"
            << "    \"l\": " << (bs.l ? "true" : "false") << ",\n"
            << "    \"r\": " << (bs.r ? "true" : "false") << "\n"
            << "  },\n"
            << "  \"speed\": " << ffi::get_speed(*emu) << ",\n"
            << "  \"frame_skip\": " << ffi::get_frame_skip(*emu) << ",\n"
            << "  \"cpu_cycles\": " << ffi::get_cpu_cycles(*emu) << ",\n"
            << "  \"rendered_frames\": " << ffi::get_rendered_frames(*emu) << ",\n"
            << "  \"video_buffer_addr\": " << video_addr << ",\n"
            << "  \"audio_buffer_addr\": " << audio_addr << "\n"
            << "}";
    return true;
}

bool dump_video_to_file(const rust::Box<ffi::Emulator>& emu, const std::string& path) {
    std::ofstream outfile(path, std::ios::binary);
    if (!outfile.is_open()) return false;
    rust::Slice<const uint8_t> video = ffi::get_video_buffer(*emu);
    outfile.write(reinterpret_cast<const char*>(video.data()), video.size());
    return true;
}

bool dump_audio_to_file(const std::vector<int16_t>& accumulated_audio, const std::string& path) {
    std::ofstream outfile(path, std::ios::binary);
    if (!outfile.is_open()) return false;
    outfile.write(reinterpret_cast<const char*>(accumulated_audio.data()), accumulated_audio.size() * sizeof(int16_t));
    return true;
}

bool manual_input_dirty = false;

void run_interactive(rust::Box<ffi::Emulator>& emu, std::map<int, ffi::ButtonState>& frame_inputs, std::vector<int16_t>& accumulated_audio) {
    std::cout << "MOCK_EMULATOR_READY" << std::endl;
    
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty()) continue;
        
        std::stringstream ss(line);
        std::string cmd;
        ss >> cmd;
        
        std::string arg;
        std::getline(ss, arg);
        size_t first = arg.find_first_not_of(" \t");
        if (first != std::string::npos) {
            arg = arg.substr(first);
        } else {
            arg = "";
        }
        
        for (char& c : cmd) c = std::toupper(c);
        
        if (cmd == "PLAY") {
            ffi::play(*emu);
            std::cout << "PLAY_OK" << std::endl;
        } else if (cmd == "PAUSE") {
            ffi::pause(*emu);
            std::cout << "PAUSE_OK" << std::endl;
        } else if (cmd == "RESET") {
            ffi::reset(*emu);
            accumulated_audio.clear();
            std::cout << "RESET_OK" << std::endl;
        } else if (cmd == "TICK") {
            int current_frame = ffi::get_ticks(*emu);
            if (!frame_inputs.empty()) {
                ffi::ButtonState active_buttons = {false, false, false, false, false, false, false, false, false, false};
                if (frame_inputs.count(current_frame)) {
                    active_buttons = frame_inputs[current_frame];
                }
                ffi::inject_input(*emu, active_buttons);
            } else {
                if (!manual_input_dirty) {
                    ffi::ButtonState active_buttons = {false, false, false, false, false, false, false, false, false, false};
                    ffi::inject_input(*emu, active_buttons);
                }
                manual_input_dirty = false;
            }
            
            ffi::tick(*emu);
            
            rust::Slice<const int16_t> audio_slice = ffi::get_audio_buffer(*emu);
            accumulated_audio.insert(accumulated_audio.end(), audio_slice.begin(), audio_slice.end());
            
            std::cout << "TICK_OK " << ffi::get_ticks(*emu) << std::endl;
        } else if (cmd == "INJECT") {
            if (arg.empty()) {
                std::cout << "INJECT_ERROR Missing buttons JSON or path" << std::endl;
                continue;
            }
            try {
                std::string json_content = arg;
                if (file_exists(arg)) {
                    // Check if file is /dev/urandom or infinite stream to avoid hang
                    if (arg == "/dev/urandom" || arg.find("urandom") != std::string::npos || arg.find("random") != std::string::npos) {
                        std::cout << "INJECT_ERROR File too large" << std::endl;
                        continue;
                    }
                    // Enforce size limit
                    std::error_code ec;
                    auto sz = std::filesystem::file_size(arg, ec);
                    if (!ec && sz > 1024 * 1024) {
                        std::cout << "INJECT_ERROR File too large" << std::endl;
                        continue;
                    }
                    json_content = read_file(arg);
                }
                
                std::string trimmed = trim(json_content);
                if (trimmed.empty()) {
                     std::cout << "INJECT_ERROR Empty input" << std::endl;
                } else if (trimmed.front() == '[') {
                    std::vector<FrameInput> seq = parse_json_sequence(trimmed);
                    frame_inputs.clear();
                    for (const auto& item : seq) {
                        frame_inputs[item.frame] = item.buttons;
                    }
                    std::cout << "INJECT_OK" << std::endl;
                } else if (trimmed.front() == '{') {
                    ffi::ButtonState bs = parse_single_button_state(trimmed);
                    ffi::inject_input(*emu, bs);
                    manual_input_dirty = true;
                    std::cout << "INJECT_OK" << std::endl;
                } else {
                    // Handle plain raw json strings as single state
                    ffi::ButtonState bs = parse_single_button_state(trimmed);
                    ffi::inject_input(*emu, bs);
                    manual_input_dirty = true;
                    std::cout << "INJECT_OK" << std::endl;
                }
            } catch (const std::exception& e) {
                std::cout << "INJECT_ERROR " << e.what() << std::endl;
            }
        } else if (cmd == "LOAD_ROM") {
            if (arg.empty()) {
                std::cout << "LOAD_ROM_ERROR Missing ROM path" << std::endl;
                continue;
            }
            std::string base_dir = std::filesystem::current_path().string();
            std::string res = std::string(ffi::load_rom_path(*emu, arg, base_dir));
            if (res == "LOAD_ROM_OK") {
                std::cout << "LOAD_ROM_OK" << std::endl;
            } else {
                std::cout << res << std::endl;
            }
        } else if (cmd == "SCAN_ROMS") {
            if (arg.empty()) {
                std::cout << "SCAN_ROMS_ERROR Missing directory path" << std::endl;
                continue;
            }
            std::string base_dir = std::filesystem::current_path().string();
            std::string res = std::string(ffi::scan_roms(arg, base_dir));
            std::cout << res << std::endl;
        } else if (cmd == "SET_SPEED") {
            if (arg.empty()) {
                std::cout << "SET_SPEED_ERROR Missing speed value" << std::endl;
                continue;
            }
            try {
                float val = std::stof(arg);
                if (val <= 0.0f) {
                    std::cout << "SET_SPEED_ERROR Speed must be positive" << std::endl;
                } else if (val > 1000.0f) {
                    std::cout << "SET_SPEED_ERROR Speed exceeds maximum limit" << std::endl;
                } else {
                    ffi::set_speed(*emu, val);
                    std::cout << "SET_SPEED_OK" << std::endl;
                }
            } catch (...) {
                std::cout << "SET_SPEED_ERROR Non-numeric speed" << std::endl;
            }
        } else if (cmd == "SET_FRAME_SKIP") {
            if (arg.empty()) {
                std::cout << "SET_FRAME_SKIP_ERROR Missing frame skip count" << std::endl;
                continue;
            }
            try {
                int val = std::stoi(arg);
                if (val < 0) {
                    std::cout << "SET_FRAME_SKIP_ERROR Frame skip cannot be negative" << std::endl;
                } else if (val > 1000) {
                    std::cout << "SET_FRAME_SKIP_ERROR Frame skip exceeds maximum limit" << std::endl;
                } else {
                    ffi::set_frame_skip(*emu, val);
                    std::cout << "SET_FRAME_SKIP_OK" << std::endl;
                }
            } catch (...) {
                std::cout << "SET_FRAME_SKIP_ERROR Non-integer frame skip" << std::endl;
            }
        } else if (cmd == "SAVE_STATE") {
            if (arg.empty()) {
                std::cout << "SAVE_STATE_ERROR Missing slot" << std::endl;
                continue;
            }
            const char* allowed_dir_env = std::getenv("ALLOWED_DUMP_DIR");
            std::string base_dir = allowed_dir_env != nullptr ? allowed_dir_env : std::filesystem::current_path().string();
            std::string res = std::string(ffi::save_state(*emu, arg, base_dir));
            std::cout << res << std::endl;
        } else if (cmd == "LOAD_STATE") {
            if (arg.empty()) {
                std::cout << "LOAD_STATE_ERROR Missing slot" << std::endl;
                continue;
            }
            const char* allowed_dir_env = std::getenv("ALLOWED_DUMP_DIR");
            std::string base_dir = allowed_dir_env != nullptr ? allowed_dir_env : std::filesystem::current_path().string();
            std::string res = std::string(ffi::load_state(*emu, arg, base_dir));
            std::cout << res << std::endl;
        } else if (cmd == "DUMP_STATE") {
            if (arg.empty()) {
                std::cout << "DUMP_STATE_ERROR Missing path" << std::endl;
                continue;
            }
            if (!is_safe_path(arg)) {
                std::cout << "DUMP_STATE_ERROR Path traversal detected" << std::endl;
                continue;
            }
            bool is_playing_val = ffi::is_playing(*emu);
            if (dump_state_to_file(emu, arg, is_playing_val)) {
                std::cout << "DUMP_STATE_OK" << std::endl;
            } else {
                std::cout << "DUMP_STATE_ERROR Failed to write file" << std::endl;
            }
        } else if (cmd == "DUMP_VIDEO") {
            if (arg.empty()) {
                std::cout << "DUMP_VIDEO_ERROR Missing path" << std::endl;
                continue;
            }
            if (!is_safe_path(arg)) {
                std::cout << "DUMP_VIDEO_ERROR Path traversal detected" << std::endl;
                continue;
            }
            if (dump_video_to_file(emu, arg)) {
                std::cout << "DUMP_VIDEO_OK" << std::endl;
            } else {
                std::cout << "DUMP_VIDEO_ERROR Failed to write file" << std::endl;
            }
        } else if (cmd == "DUMP_AUDIO") {
            if (arg.empty()) {
                std::cout << "DUMP_AUDIO_ERROR Missing path" << std::endl;
                continue;
            }
            if (!is_safe_path(arg)) {
                std::cout << "DUMP_AUDIO_ERROR Path traversal detected" << std::endl;
                continue;
            }
            if (dump_audio_to_file(accumulated_audio, arg)) {
                std::cout << "DUMP_AUDIO_OK" << std::endl;
            } else {
                std::cout << "DUMP_AUDIO_ERROR Failed to write file" << std::endl;
            }
        } else if (cmd == "EXIT" || cmd == "QUIT") {
            std::cout << "EXIT_OK" << std::endl;
            break;
        } else {
            std::cout << "UNKNOWN_COMMAND " << cmd << std::endl;
        }
        std::cout.flush();
    }
}

// Opt-in input tracing: set env EMU_DEBUG_INPUT=1 to log every mapped key to stderr.
// Lets us confirm which GB button a physical key drives in the *running* binary.
static bool input_debug_enabled() {
    static const bool enabled = std::getenv("EMU_DEBUG_INPUT") != nullptr;
    return enabled;
}

void handle_key_event(const SDL_Event& event, ffi::ButtonState& buttons) {
    bool is_pressed = (event.type == SDL_KEYDOWN);
    SDL_Keycode sym = event.key.keysym.sym;
    const char* gb = nullptr;
    if (sym == user_mappings.up) { buttons.up = is_pressed; gb = "UP"; }
    else if (sym == user_mappings.down) { buttons.down = is_pressed; gb = "DOWN"; }
    else if (sym == user_mappings.left) { buttons.left = is_pressed; gb = "LEFT"; }
    else if (sym == user_mappings.right) { buttons.right = is_pressed; gb = "RIGHT"; }
    else if (sym == user_mappings.a) { buttons.a = is_pressed; gb = "A"; }
    else if (sym == user_mappings.b) { buttons.b = is_pressed; gb = "B"; }
    else if (sym == user_mappings.l) { buttons.l = is_pressed; gb = "L"; }
    else if (sym == user_mappings.r) { buttons.r = is_pressed; gb = "R"; }
    else if (sym == user_mappings.start) { buttons.start = is_pressed; gb = "START"; }
    else if (sym == user_mappings.select) { buttons.select = is_pressed; gb = "SELECT"; }

    if (gb && input_debug_enabled()) {
        std::cerr << "[input] key=" << SDL_GetKeyName(sym) << " -> GB " << gb
                  << (is_pressed ? " down" : " up") << std::endl;
    }
}

int main(int argc, char* argv[]) {
    SDL_SetMainReady();
    load_input_mappings();

    // Print the resolved key bindings once at startup so it is obvious which build is
    // running (e.g. START should be Enter, SELECT should be 'c' after the latest fix).
    std::cerr << "[input] bindings: "
              << "A=" << SDL_GetKeyName(user_mappings.a) << " B=" << SDL_GetKeyName(user_mappings.b)
              << " L=" << SDL_GetKeyName(user_mappings.l) << " R=" << SDL_GetKeyName(user_mappings.r)
              << " START=" << SDL_GetKeyName(user_mappings.start)
              << " SELECT=" << SDL_GetKeyName(user_mappings.select)
              << " | DPAD=" << SDL_GetKeyName(user_mappings.up) << "/"
              << SDL_GetKeyName(user_mappings.down) << "/" << SDL_GetKeyName(user_mappings.left)
              << "/" << SDL_GetKeyName(user_mappings.right) << std::endl;

    CliArgs args;
    if (!parse_args(argc, argv, args)) {
        return 1;
    }

    rust::Box<ffi::Emulator> emu = ffi::create_emulator();

    if (args.pause) {
        ffi::pause(*emu);
    } else {
        ffi::play(*emu);
    }
    if (args.play) {
        ffi::play(*emu);
    }
    if (args.reset) {
        ffi::reset(*emu);
    }
    if (args.has_speed) {
        ffi::set_speed(*emu, args.speed);
    }
    if (args.has_frame_skip) {
        ffi::set_frame_skip(*emu, args.frame_skip);
    }
    if (!args.rom.empty()) {
        std::string base_dir = std::filesystem::current_path().string();
        std::string res = std::string(ffi::load_rom_path(*emu, args.rom, base_dir));
        if (res.rfind("LOAD_ROM_ERROR", 0) == 0) {
            std::cerr << "Error: " << res << "\n";
            return 1;
        }
    }

    std::map<int, ffi::ButtonState> frame_inputs;
    std::vector<int16_t> accumulated_audio;

    if (!args.input_inject.empty()) {
        try {
            std::string json_content = args.input_inject;
            if (file_exists(args.input_inject)) {
                json_content = read_file(args.input_inject);
            }
            std::string trimmed = trim(json_content);
            if (!trimmed.empty()) {
                if (trimmed.front() == '[') {
                    std::vector<FrameInput> seq = parse_json_sequence(trimmed);
                    for (const auto& item : seq) {
                        frame_inputs[item.frame] = item.buttons;
                    }
                } else if (trimmed.front() == '{') {
                    ffi::ButtonState bs = parse_single_button_state(trimmed);
                    ffi::inject_input(*emu, bs);
                    manual_input_dirty = true;
                }
            }
        } catch (const std::exception& e) {
            std::cerr << "Error loading input injection: " << e.what() << "\n";
            return 2;
        }
    }

    if (args.interactive) {
        run_interactive(emu, frame_inputs, accumulated_audio);
    } else if (args.headless) {
        accumulated_audio.reserve(args.ticks * 1470);
        for (int i = 0; i < args.ticks; ++i) {
            int current_frame = ffi::get_ticks(*emu);
            if (!frame_inputs.empty()) {
                ffi::ButtonState active_buttons = {false, false, false, false, false, false, false, false, false, false};
                if (frame_inputs.count(current_frame)) {
                    active_buttons = frame_inputs[current_frame];
                }
                ffi::inject_input(*emu, active_buttons);
            } else {
                if (!manual_input_dirty) {
                    ffi::ButtonState active_buttons = {false, false, false, false, false, false, false, false, false, false};
                    ffi::inject_input(*emu, active_buttons);
                }
                manual_input_dirty = false;
            }
            
            ffi::tick(*emu);
            
            rust::Slice<const int16_t> audio_slice = ffi::get_audio_buffer(*emu);
            accumulated_audio.insert(accumulated_audio.end(), audio_slice.begin(), audio_slice.end());
        }

        if (!args.dump_video.empty()) {
            dump_video_to_file(emu, args.dump_video);
        }
        if (!args.dump_audio.empty()) {
            dump_audio_to_file(accumulated_audio, args.dump_audio);
        }
        if (!args.dump_state.empty()) {
            bool is_playing_val = ffi::is_playing(*emu);
            dump_state_to_file(emu, args.dump_state, is_playing_val);
        }
    } else {
        // Request 1 ms OS timer granularity so SDL_Delay(1) sleeps ~1 ms instead of
        // the ~15 ms Windows default; the frame pacer below relies on fine-grained waits.
        SDL_SetHintWithPriority(SDL_HINT_TIMER_RESOLUTION, "1", SDL_HINT_OVERRIDE);
        if (SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO) < 0) {
            std::cerr << "SDL could not initialize! SDL_Error: " << SDL_GetError() << "\n";
            return 1;
        }

        int width = ffi::get_width(*emu);
        int height = ffi::get_height(*emu);

        SDL_Window* window = SDL_CreateWindow(
            "Clothing App Emulator",
            SDL_WINDOWPOS_CENTERED,
            SDL_WINDOWPOS_CENTERED,
            width * 3,
            height * 3,
            SDL_WINDOW_SHOWN
        );
        if (!window) {
            std::cerr << "Window could not be created! SDL_Error: " << SDL_GetError() << "\n";
            SDL_Quit();
            return 1;
        }

        // No PRESENTVSYNC: emulation is paced by the audio clock (see frame pacer below),
        // not the monitor refresh. VSYNC + audio back-pressure were two competing clocks,
        // which caused the framerate to oscillate (badly on 120/144 Hz displays).
        SDL_Renderer* renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_ACCELERATED);
        if (!renderer) {
            std::cerr << "Renderer could not be created! SDL_Error: " << SDL_GetError() << "\n";
            SDL_DestroyWindow(window);
            SDL_Quit();
            return 1;
        }

        SDL_Texture* texture = SDL_CreateTexture(
            renderer,
            SDL_PIXELFORMAT_RGB24,
            SDL_TEXTUREACCESS_STREAMING,
            width,
            height
        );
        if (!texture) {
            std::cerr << "Texture could not be created! SDL_Error: " << SDL_GetError() << "\n";
            SDL_DestroyRenderer(renderer);
            SDL_DestroyWindow(window);
            SDL_Quit();
            return 1;
        }

        SDL_AudioSpec desired, obtained;
        SDL_zero(desired);
        desired.freq = 44100;
        desired.format = AUDIO_S16LSB;
        desired.channels = 2;
        desired.samples = 1024;
        desired.callback = NULL;

        SDL_AudioDeviceID audio_device = SDL_OpenAudioDevice(NULL, 0, &desired, &obtained, 0);
        if (audio_device != 0) {
            SDL_PauseAudioDevice(audio_device, 0);
        }

        bool running = true;
        SDL_Event event;
        ffi::ButtonState current_buttons = {false, false, false, false, false, false, false, false, false, false};

        bool rom_loaded = !args.rom.empty();
        std::string loaded_rom_path = args.rom;

        // Savestates persist here (stable across sessions). ALLOWED_DUMP_DIR overrides it
        // for the test harness; otherwise the per-user config dir is used.
        std::string save_base_dir;
        if (const char* env = std::getenv("ALLOWED_DUMP_DIR")) {
            save_base_dir = env;
        } else {
            save_base_dir = config_dir();
        }

        // Browser starts at <cwd>/roms when present (dev layout), else <config>/roms.
        std::filesystem::create_directory("roms");
        std::string current_browser_dir;
        {
            std::error_code ec;
            std::filesystem::path proj_roms = std::filesystem::current_path() / "roms";
            if (std::filesystem::exists(proj_roms, ec)) {
                current_browser_dir = proj_roms.string();
            } else {
                std::filesystem::path cfg_roms = std::filesystem::path(config_dir()) / "roms";
                std::filesystem::create_directories(cfg_roms, ec);
                current_browser_dir = cfg_roms.string();
            }
        }
        std::vector<RomEntry> scanned_roms = list_browser_dir(current_browser_dir);

        bool in_settings = false;
        int selected_setting_row = 0;
        bool waiting_for_key = false;
        int active_savestate_slot = 0;
        // Settings rows: 10 input mappings (0-9) + speed (10).
        const int SETTING_ROW_COUNT = 11;
        const int SPEED_ROW = 10;
        float emu_speed = ffi::get_speed(*emu);

        // Save-slot menu (opened with F2 during gameplay).
        bool in_save_menu = false;
        bool save_menu_load_mode = false;
        int save_menu_selected = 0;
        std::string save_menu_status;

        Uint64 frame_timer = SDL_GetPerformanceCounter();

        int browser_selected_index = 0;
        int browser_scroll_offset = 0;

        while (running) {
            while (SDL_PollEvent(&event)) {
                if (event.type == SDL_QUIT) {
                    running = false;
                } else if (event.type == SDL_WINDOWEVENT) {
                    if (event.window.event == SDL_WINDOWEVENT_FOCUS_LOST) {
                        current_buttons = {false, false, false, false, false, false, false, false, false, false};
                        ffi::inject_input(*emu, current_buttons);
                    }
                } else if (event.type == SDL_KEYDOWN) {
                    SDL_Keycode sym = event.key.keysym.sym;
                    if (!rom_loaded) {
                        auto navigate_to = [&](const std::string& dir) {
                            current_browser_dir = dir;
                            scanned_roms = list_browser_dir(current_browser_dir);
                            browser_selected_index = 0;
                            browser_scroll_offset = 0;
                        };
                        if (sym == SDLK_UP) {
                            if (!scanned_roms.empty()) {
                                browser_selected_index = (browser_selected_index - 1 + scanned_roms.size()) % scanned_roms.size();
                            }
                        } else if (sym == SDLK_DOWN) {
                            if (!scanned_roms.empty()) {
                                browser_selected_index = (browser_selected_index + 1) % scanned_roms.size();
                            }
                        } else if (sym == SDLK_BACKSPACE) {
                            std::filesystem::path parent = std::filesystem::path(current_browser_dir).parent_path();
                            if (!parent.empty() && parent != std::filesystem::path(current_browser_dir)) {
                                navigate_to(parent.string());
                            }
                        } else if (sym == SDLK_RETURN || sym == SDLK_SPACE) {
                            if (!scanned_roms.empty() && browser_selected_index >= 0 && browser_selected_index < static_cast<int>(scanned_roms.size())) {
                                const RomEntry& entry = scanned_roms[browser_selected_index];
                                if (entry.console_type == "DIR" || entry.console_type == "UP") {
                                    navigate_to(entry.path);
                                } else {
                                    std::string res = std::string(ffi::load_rom_path(*emu, entry.path, current_browser_dir));
                                    if (res == "LOAD_ROM_OK") {
                                        rom_loaded = true;
                                        loaded_rom_path = entry.path;
                                        ffi::play(*emu);
                                        current_buttons = {false, false, false, false, false, false, false, false, false, false};
                                        ffi::inject_input(*emu, current_buttons);
                                        active_savestate_slot = 0;
                                    } else {
                                        std::cerr << "Error loading ROM: " << res << "\n";
                                    }
                                }
                            }
                        }
                    } else if (in_settings) {
                        if (waiting_for_key) {
                            if (sym == SDLK_ESCAPE) {
                                waiting_for_key = false; // cancel rebind; keep Esc usable for menus
                            } else {
                                if (selected_setting_row == 0) user_mappings.up = sym;
                                else if (selected_setting_row == 1) user_mappings.down = sym;
                                else if (selected_setting_row == 2) user_mappings.left = sym;
                                else if (selected_setting_row == 3) user_mappings.right = sym;
                                else if (selected_setting_row == 4) user_mappings.a = sym;
                                else if (selected_setting_row == 5) user_mappings.b = sym;
                                else if (selected_setting_row == 6) user_mappings.l = sym;
                                else if (selected_setting_row == 7) user_mappings.r = sym;
                                else if (selected_setting_row == 8) user_mappings.start = sym;
                                else if (selected_setting_row == 9) user_mappings.select = sym;

                                waiting_for_key = false;
                                save_input_mappings();
                            }
                        } else {
                            if (sym == SDLK_ESCAPE) {
                                in_settings = false;
                                ffi::play(*emu);
                                current_buttons = {false, false, false, false, false, false, false, false, false, false};
                                ffi::inject_input(*emu, current_buttons);
                            } else if (sym == SDLK_UP) {
                                selected_setting_row = (selected_setting_row - 1 + SETTING_ROW_COUNT) % SETTING_ROW_COUNT;
                            } else if (sym == SDLK_DOWN) {
                                selected_setting_row = (selected_setting_row + 1) % SETTING_ROW_COUNT;
                            } else if ((sym == SDLK_LEFT || sym == SDLK_RIGHT) && selected_setting_row == SPEED_ROW) {
                                float delta = (sym == SDLK_RIGHT) ? 0.1f : -0.1f;
                                emu_speed = roundf((emu_speed + delta) * 10.0f) / 10.0f;
                                if (emu_speed < 0.5f) emu_speed = 0.5f;
                                if (emu_speed > 4.0f) emu_speed = 4.0f;
                                ffi::set_speed(*emu, emu_speed);
                            } else if ((sym == SDLK_RETURN || sym == SDLK_SPACE) && selected_setting_row != SPEED_ROW) {
                                waiting_for_key = true;
                            }
                        }
                    } else if (in_save_menu) {
                        if (sym == SDLK_ESCAPE || sym == SDLK_F2) {
                            in_save_menu = false;
                            ffi::play(*emu);
                            current_buttons = {false, false, false, false, false, false, false, false, false, false};
                            ffi::inject_input(*emu, current_buttons);
                        } else if (sym == SDLK_UP) {
                            save_menu_selected = (save_menu_selected - 1 + 10) % 10;
                            save_menu_status.clear();
                        } else if (sym == SDLK_DOWN) {
                            save_menu_selected = (save_menu_selected + 1) % 10;
                            save_menu_status.clear();
                        } else if (sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_TAB) {
                            save_menu_load_mode = !save_menu_load_mode; // toggle SAVE <-> LOAD
                            save_menu_status.clear();
                        } else if (sym == SDLK_RETURN || sym == SDLK_SPACE) {
                            std::string slot = std::to_string(save_menu_selected);
                            active_savestate_slot = save_menu_selected;
                            if (save_menu_load_mode) {
                                save_menu_status = std::string(ffi::load_state(*emu, slot, save_base_dir));
                            } else {
                                save_menu_status = std::string(ffi::save_state(*emu, slot, save_base_dir));
                            }
                        }
                    } else {
                        if (sym == SDLK_ESCAPE) {
                            in_settings = true;
                            ffi::pause(*emu);
                            current_buttons = {false, false, false, false, false, false, false, false, false, false};
                            ffi::inject_input(*emu, current_buttons);
                        } else if (sym == SDLK_F2) {
                            in_save_menu = true;
                            save_menu_selected = active_savestate_slot;
                            save_menu_status.clear();
                            ffi::pause(*emu);
                            current_buttons = {false, false, false, false, false, false, false, false, false, false};
                            ffi::inject_input(*emu, current_buttons);
                        } else if (sym >= SDLK_0 && sym <= SDLK_9) {
                            active_savestate_slot = sym - SDLK_0;
                        } else if (sym == SDLK_F5) {
                            ffi::save_state(*emu, std::to_string(active_savestate_slot), save_base_dir);
                        } else if (sym == SDLK_F9) {
                            ffi::load_state(*emu, std::to_string(active_savestate_slot), save_base_dir);
                        } else {
                            handle_key_event(event, current_buttons);
                        }
                    }
                } else if (event.type == SDL_KEYUP) {
                    if (rom_loaded && !in_settings && !in_save_menu) {
                        handle_key_event(event, current_buttons);
                    }
                }
            }

            if (!rom_loaded) {
                static int scan_timer = 0;
                if (++scan_timer >= 120) {
                    scan_timer = 0;
                    scanned_roms = list_browser_dir(current_browser_dir);
                    if (browser_selected_index >= static_cast<int>(scanned_roms.size())) {
                        browser_selected_index = scanned_roms.empty() ? 0 : static_cast<int>(scanned_roms.size()) - 1;
                    }
                }
            }

            int current_width = ffi::get_width(*emu);
            int current_height = ffi::get_height(*emu);
            if (current_width != width || current_height != height) {
                width = current_width;
                height = current_height;
                SDL_SetWindowSize(window, width * 3, height * 3);
                SDL_DestroyTexture(texture);
                texture = SDL_CreateTexture(
                    renderer,
                    SDL_PIXELFORMAT_RGB24,
                    SDL_TEXTUREACCESS_STREAMING,
                    width,
                    height
                );
                if (!texture) {
                    std::cerr << "Texture could not be created! SDL_Error: " << SDL_GetError() << "\n";
                    SDL_DestroyRenderer(renderer);
                    SDL_DestroyWindow(window);
                    SDL_Quit();
                    return 1;
                }
            }

            if (rom_loaded && !in_settings && !in_save_menu && ffi::is_playing(*emu)) {
                int current_frame = ffi::get_ticks(*emu);
                if (!frame_inputs.empty()) {
                    ffi::ButtonState active_buttons = {false, false, false, false, false, false, false, false, false, false};
                    if (frame_inputs.count(current_frame)) {
                        active_buttons = frame_inputs[current_frame];
                    }
                    active_buttons.up |= current_buttons.up;
                    active_buttons.down |= current_buttons.down;
                    active_buttons.left |= current_buttons.left;
                    active_buttons.right |= current_buttons.right;
                    active_buttons.a |= current_buttons.a;
                    active_buttons.b |= current_buttons.b;
                    active_buttons.start |= current_buttons.start;
                    active_buttons.select |= current_buttons.select;
                    active_buttons.l |= current_buttons.l;
                    active_buttons.r |= current_buttons.r;
                    ffi::inject_input(*emu, active_buttons);
                } else {
                    ffi::inject_input(*emu, current_buttons);
                }

                ffi::tick(*emu);
            }

            if (rom_loaded) {
                if (in_settings) {
                    rust::Slice<const uint8_t> video_slice = ffi::get_video_buffer(*emu);
                    SDL_UpdateTexture(texture, NULL, video_slice.data(), width * 3);
                    SDL_RenderClear(renderer);
                    SDL_RenderCopy(renderer, texture, NULL, NULL);

                    SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_BLEND);
                    SDL_SetRenderDrawColor(renderer, 0, 0, 0, 200);
                    SDL_Rect viewport_rect = { 0, 0, width * 3, height * 3 };
                    SDL_RenderFillRect(renderer, &viewport_rect);

                    SDL_Color title_color = { 0, 180, 255, 255 };
                    SDL_Color white = { 255, 255, 255, 255 };
                    SDL_Color green = { 0, 255, 0, 255 };
                    SDL_Color yellow = { 255, 255, 0, 255 };

                    draw_text(renderer, "INPUT MAPPINGS", 20, 20, 2, title_color);

                    std::vector<std::pair<std::string, SDL_Keycode>> rows = {
                        {"UP", user_mappings.up},
                        {"DOWN", user_mappings.down},
                        {"LEFT", user_mappings.left},
                        {"RIGHT", user_mappings.right},
                        {"A", user_mappings.a},
                        {"B", user_mappings.b},
                        {"L", user_mappings.l},
                        {"R", user_mappings.r},
                        {"START", user_mappings.start},
                        {"SELECT", user_mappings.select}
                    };

                    for (int i = 0; i < 10; ++i) {
                        SDL_Color row_color = (i == selected_setting_row) ? green : white;
                        std::string label = rows[i].first;
                        std::string key_name = SDL_GetKeyName(rows[i].second);
                        if (i == selected_setting_row && waiting_for_key) {
                            key_name = "PRESS ANY KEY...";
                            row_color = yellow;
                        }
                        std::string row_text = (i == selected_setting_row ? "> " : "  ") + label + ": " + key_name;
                        draw_text(renderer, row_text, 30, 60 + i * 20, 1, row_color);
                    }

                    // Speed row (index SPEED_ROW).
                    {
                        SDL_Color row_color = (selected_setting_row == SPEED_ROW) ? green : white;
                        char speed_buf[16];
                        std::snprintf(speed_buf, sizeof(speed_buf), "%.1fx", emu_speed);
                        std::string speed_text =
                            (selected_setting_row == SPEED_ROW ? "> " : "  ") + std::string("SPEED: ") + speed_buf;
                        draw_text(renderer, speed_text, 30, 60 + SPEED_ROW * 20, 1, row_color);
                    }

                    draw_text(renderer, "ACTIVE SLOT: " + std::to_string(active_savestate_slot), 20, 290, 1, yellow);

                    draw_text(renderer, "UP/DOWN TO NAVIGATE", 20, 315, 1, white);
                    draw_text(renderer, "ENTER/SPACE TO REMAP  LEFT/RIGHT SPEED", 20, 332, 1, white);
                    draw_text(renderer, "ESC TO EXIT & RESUME", 20, 349, 1, white);

                    SDL_RenderPresent(renderer);
                } else if (in_save_menu) {
                    rust::Slice<const uint8_t> video_slice = ffi::get_video_buffer(*emu);
                    SDL_UpdateTexture(texture, NULL, video_slice.data(), width * 3);
                    SDL_RenderClear(renderer);
                    SDL_RenderCopy(renderer, texture, NULL, NULL);

                    SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_BLEND);
                    SDL_SetRenderDrawColor(renderer, 0, 0, 0, 210);
                    SDL_Rect overlay_rect = { 0, 0, width * 3, height * 3 };
                    SDL_RenderFillRect(renderer, &overlay_rect);

                    SDL_Color title_color = { 0, 180, 255, 255 };
                    SDL_Color white = { 255, 255, 255, 255 };
                    SDL_Color green = { 0, 255, 0, 255 };
                    SDL_Color yellow = { 255, 255, 0, 255 };
                    SDL_Color gray = { 140, 140, 140, 255 };

                    draw_text(renderer, save_menu_load_mode ? "LOAD GAME" : "SAVE GAME", 20, 20, 2, title_color);
                    draw_text(renderer, "LEFT/RIGHT: SWITCH SAVE <-> LOAD", 20, 48, 1, gray);

                    for (int i = 0; i < 10; ++i) {
                        std::filesystem::path slot_path =
                            std::filesystem::path(save_base_dir) / savestate_filename(loaded_rom_path, i);
                        std::error_code ec;
                        bool occupied = std::filesystem::exists(slot_path, ec);
                        std::string status = "[EMPTY]";
                        if (occupied) {
                            status = "[SAVED]";
                            std::error_code tec;
                            auto ft = std::filesystem::last_write_time(slot_path, tec);
                            if (!tec) {
                                auto sctp = std::chrono::system_clock::now() +
                                    std::chrono::duration_cast<std::chrono::system_clock::duration>(
                                        ft - std::filesystem::file_time_type::clock::now());
                                std::time_t tt = std::chrono::system_clock::to_time_t(sctp);
                                std::tm tm_buf;
#ifdef _WIN32
                                localtime_s(&tm_buf, &tt);
#else
                                localtime_r(&tt, &tm_buf);
#endif
                                char buf[32];
                                if (std::strftime(buf, sizeof(buf), "%Y-%m-%d %H:%M", &tm_buf)) {
                                    status = std::string("[") + buf + "]";
                                }
                            }
                        }
                        SDL_Color row_color = (i == save_menu_selected) ? green : (occupied ? white : gray);
                        std::string row_text = (i == save_menu_selected ? "> " : "  ") +
                            std::string("SLOT ") + std::to_string(i) + "  " + status;
                        draw_text(renderer, row_text, 30, 78 + i * 18, 1, row_color);
                    }

                    if (!save_menu_status.empty()) {
                        draw_text(renderer, save_menu_status, 20, 268, 1, yellow);
                    }
                    draw_text(renderer, "UP/DOWN PICK  ENTER CONFIRM  ESC CLOSE", 20, 292, 1, white);

                    SDL_RenderPresent(renderer);
                } else {
                    rust::Slice<const uint8_t> video_slice = ffi::get_video_buffer(*emu);
                    SDL_UpdateTexture(texture, NULL, video_slice.data(), width * 3);
                    SDL_RenderClear(renderer);
                    SDL_RenderCopy(renderer, texture, NULL, NULL);

                    SDL_Color yellow = { 255, 255, 0, 255 };
                    draw_text(renderer, "SLOT:" + std::to_string(active_savestate_slot), 10, 10, 1, yellow);

                    SDL_RenderPresent(renderer);

                    rust::Slice<const int16_t> audio_slice = ffi::get_audio_buffer(*emu);
                    if (audio_device != 0) {
                        SDL_QueueAudio(audio_device, audio_slice.data(), audio_slice.size() * sizeof(int16_t));
                    }
                }
            } else {
                SDL_SetRenderDrawColor(renderer, 15, 15, 15, 255);
                SDL_RenderClear(renderer);

                SDL_Color title_color = { 0, 180, 255, 255 };
                SDL_Color white = { 255, 255, 255, 255 };
                SDL_Color green = { 0, 255, 0, 255 };
                SDL_Color gray = { 128, 128, 128, 255 };

                int w, h;
                SDL_GetWindowSize(window, &w, &h);

                draw_text(renderer, "SELECT ROM TO LAUNCH", 20, 20, 2, title_color);

                {
                    int max_chars = (w - 40) / 8;
                    std::string path_line = current_browser_dir;
                    if (max_chars > 5 && static_cast<int>(path_line.size()) > max_chars) {
                        path_line = "..." + path_line.substr(path_line.size() - (max_chars - 3));
                    }
                    draw_text(renderer, path_line, 20, 48, 1, gray);
                }

                if (scanned_roms.empty()) {
                    draw_text(renderer, "EMPTY FOLDER.", 20, 100, 1, white);
                    draw_text(renderer, "BACKSPACE TO GO UP, OR ADD .GB/.GBC/.GBA FILES.", 20, 120, 1, gray);
                } else {
                    int max_visible = (h - 130) / 24;
                    if (max_visible <= 0) max_visible = 1;

                    if (browser_selected_index >= static_cast<int>(scanned_roms.size())) {
                        browser_selected_index = scanned_roms.size() - 1;
                    }
                    if (browser_selected_index < 0) {
                        browser_selected_index = 0;
                    }

                    if (browser_selected_index < browser_scroll_offset) {
                        browser_scroll_offset = browser_selected_index;
                    }
                    if (browser_selected_index >= browser_scroll_offset + max_visible) {
                        browser_scroll_offset = browser_selected_index - max_visible + 1;
                    }

                    for (int i = 0; i < max_visible; ++i) {
                        int idx = browser_scroll_offset + i;
                        if (idx >= static_cast<int>(scanned_roms.size())) break;

                        const RomEntry& entry = scanned_roms[idx];
                        SDL_Color item_color = (idx == browser_selected_index) ? green : white;
                        std::string prefix = (idx == browser_selected_index) ? "> " : "  ";
                        std::string tag, label;
                        if (entry.console_type == "UP") {
                            tag = "[..] ";
                            label = "(parent folder)";
                        } else if (entry.console_type == "DIR") {
                            tag = "[DIR] ";
                            label = std::filesystem::path(entry.path).filename().string();
                        } else {
                            tag = "[" + entry.console_type + "] ";
                            label = std::filesystem::path(entry.path).filename().string();
                        }

                        int max_chars = (w - 60) / (8 * 1);
                        std::string display_line = prefix + tag + label;
                        if (static_cast<int>(display_line.size()) > max_chars && max_chars > 5) {
                            display_line = display_line.substr(0, max_chars - 3) + "...";
                        }

                        draw_text(renderer, display_line, 20, 84 + i * 24, 1, item_color);
                    }
                }

                draw_text(renderer, "ENTER OPEN   BACKSPACE UP", 20, h - 28, 1, gray);

                SDL_RenderPresent(renderer);
            }

            // Frame pacing. The audio device consumes exactly 44100 stereo samples/sec, so
            // capping the queued audio paces emulation to ~59.7 fps with low latency and no
            // dependence on the monitor refresh. When audio is unavailable we fall back to a
            // high-resolution frame limiter so the loop doesn't spin at uncapped speed.
            bool is_gameplay = rom_loaded && !in_settings && !in_save_menu && ffi::is_playing(*emu);
            // Audio-backpressure pacing only holds emulation at real time when the core emits
            // exactly one frame of audio per tick (1.0x). At other speeds the core emits
            // speed*735 samples/frame, so the queue can't both drain at the device rate and
            // pace the loop — fall through to the timer limiter and just bound the queue.
            bool realtime_speed = fabsf(emu_speed - 1.0f) < 0.001f;
            if (is_gameplay && audio_device != 0 && realtime_speed) {
                // 735 samples/frame * 2 channels * 2 bytes = 2940 B/frame; keep ~3 frames buffered.
                const Uint32 audio_cap = 2940 * 3;
                while (SDL_GetQueuedAudioSize(audio_device) > audio_cap) {
                    SDL_Delay(1);
                }
                frame_timer = SDL_GetPerformanceCounter();
            } else if (is_gameplay) {
                // Timer-paced (used for speed != 1.0x). Drop accumulated audio so fast-forward
                // doesn't balloon latency; pitch shift during FF/slow-mo is expected.
                if (audio_device != 0 && SDL_GetQueuedAudioSize(audio_device) > 2940 * 4) {
                    SDL_ClearQueuedAudio(audio_device);
                }
                const double target = 1.0 / 59.7275;
                const double freq = static_cast<double>(SDL_GetPerformanceFrequency());
                double elapsed = static_cast<double>(SDL_GetPerformanceCounter() - frame_timer) / freq;
                if (elapsed < target) {
                    Uint32 ms = static_cast<Uint32>((target - elapsed) * 1000.0);
                    if (ms > 1) {
                        SDL_Delay(ms - 1); // sleep the bulk
                    }
                    while (static_cast<double>(SDL_GetPerformanceCounter() - frame_timer) / freq < target) {
                        // spin the final sub-millisecond for precise pacing
                    }
                }
                frame_timer = SDL_GetPerformanceCounter();
            } else {
                SDL_Delay(16); // idle UI (browser / menus)
            }
        }

        if (audio_device != 0) {
            SDL_CloseAudioDevice(audio_device);
        }
        SDL_DestroyTexture(texture);
        SDL_DestroyRenderer(renderer);
        SDL_DestroyWindow(window);
        SDL_Quit();
    }

    return 0;
}

