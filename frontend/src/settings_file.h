#pragma once
#include <filesystem>
#include <random>
#include <string>
#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#else
#include <fcntl.h>
#include <unistd.h>
#include <cerrno>
#endif

// Both profiles use exclusive creation and atomic replacement. Exclusive temp
// creation prevents an existing file/symlink from being followed or overwritten;
// a failed write leaves the previous profile intact and reports an actionable error.
inline std::string replace_settings_file(const std::filesystem::path& path, const std::string& data) {
    if (data.size() > 8192) return "PROFILE TOO LARGE";
    auto temporary = path;
    try {
        std::random_device random;
        temporary += ".tmp-" + std::to_string(random()) + "-" + std::to_string(random());
    } catch (const std::exception&) { return "CANNOT CREATE TEMPORARY PROFILE NAME"; }
#ifdef _WIN32
    HANDLE file = CreateFileW(temporary.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return "CANNOT CREATE PROFILE";
    DWORD written = 0;
    bool complete = WriteFile(file, data.data(), static_cast<DWORD>(data.size()), &written, nullptr) && written == data.size();
    if (complete) complete = FlushFileBuffers(file) != 0;
    if (!CloseHandle(file)) complete = false;
#else
    const int file = open(temporary.c_str(), O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (file < 0) return "CANNOT CREATE PROFILE";
    size_t written = 0;
    while (written < data.size()) {
        const auto count = write(file, data.data() + written, data.size() - written);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) break;
        written += static_cast<size_t>(count);
    }
    bool complete = written == data.size() && fsync(file) == 0;
    if (close(file) != 0) complete = false;
#endif
    std::error_code ec;
    if (!complete) { std::filesystem::remove(temporary, ec); return "CANNOT WRITE PROFILE"; }
#ifdef _WIN32
    const bool replaced = MoveFileExW(temporary.c_str(), path.c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) != 0;
#else
    std::filesystem::rename(temporary, path, ec);
    const bool replaced = !ec;
#endif
    if (!replaced) { std::filesystem::remove(temporary, ec); return "CANNOT REPLACE PROFILE"; }
    return {};
}
