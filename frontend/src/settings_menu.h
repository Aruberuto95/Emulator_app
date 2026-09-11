#pragma once
#include "n64_settings.h"
#include <vector>

enum class SettingKind {
    Key, N64Key, N64Pad, Player, Enabled, Deadzone, InvertX, InvertY, Accessory, Rumble,
    Speed, FrameSkip, Scale, Vsync, Restart, Exit,
};
struct SettingRow { SettingKind kind; const char* name; size_t control = 0; };
inline std::vector<SettingRow> settings_rows(bool n64) {
    std::vector<SettingRow> rows;
    if (n64) {
        rows.insert(rows.end(), {{SettingKind::Player, "CONTROLLER PORT"}, {SettingKind::Enabled, "PORT ENABLED"},
            {SettingKind::Accessory, "ACCESSORY"}, {SettingKind::Rumble, "VIBRATION"},
            {SettingKind::Deadzone, "STICK DEAD ZONE"}, {SettingKind::InvertX, "INVERT STICK X"},
            {SettingKind::InvertY, "INVERT STICK Y"}});
        for (size_t i = 0; i < N64_CONTROLS.size(); ++i) rows.push_back({SettingKind::N64Key, N64_CONTROLS[i].name, i});
        for (size_t i = 0; i < N64_CONTROLS.size(); ++i) rows.push_back({SettingKind::N64Pad, N64_CONTROLS[i].name, i});
    } else {
        for (size_t i = 0; i < MAPPING_KEYS.size(); ++i) rows.push_back({SettingKind::Key, MAPPING_NAMES[i], i});
    }
    rows.insert(rows.end(), {{SettingKind::Speed, "SPEED"}, {SettingKind::FrameSkip, "FRAME SKIP"},
        {SettingKind::Scale, "WINDOW SIZE"}, {SettingKind::Vsync, "VSYNC"},
        {SettingKind::Restart, "RESTART GAME"}, {SettingKind::Exit, "EXIT TO MENU"}});
    return rows;
}
struct SettingsViewport {
    static constexpr int top = 50;
    static constexpr int row_height = 17;
    int visible;
    int first;
    int footer;
    SettingsViewport(int height, int selected, int count, int previous_first)
        : visible(std::max(1, std::min(count, (height - top - 84) / row_height))),
          first(std::clamp(previous_first, 0, std::max(0, count - visible))), footer(height - 74) {
        if (selected < first) first = selected;
        if (selected >= first + visible) first = selected - visible + 1;
    }
};
