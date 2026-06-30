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
#include <SDL.h>
#include <filesystem>

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

void handle_key_event(const SDL_Event& event, ffi::ButtonState& buttons) {
    bool is_pressed = (event.type == SDL_KEYDOWN);
    switch (event.key.keysym.sym) {
        case SDLK_UP:
        case SDLK_w:
            buttons.up = is_pressed;
            break;
        case SDLK_DOWN:
        case SDLK_s:
            buttons.down = is_pressed;
            break;
        case SDLK_LEFT:
        case SDLK_a:
            buttons.left = is_pressed;
            break;
        case SDLK_RIGHT:
        case SDLK_d:
            buttons.right = is_pressed;
            break;
        case SDLK_z:
        case SDLK_j:
            buttons.a = is_pressed;
            break;
        case SDLK_x:
        case SDLK_k:
            buttons.b = is_pressed;
            break;
        case SDLK_q:
            buttons.l = is_pressed;
            break;
        case SDLK_e:
            buttons.r = is_pressed;
            break;
        case SDLK_RETURN:
            buttons.start = is_pressed;
            break;
        case SDLK_SPACE:
            buttons.select = is_pressed;
            break;
    }
}

int main(int argc, char* argv[]) {
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
        if (SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO) < 0) {
            std::cerr << "SDL could not initialize! SDL_Error: " << SDL_GetError() << "\n";
            return 1;
        }

        // Start with current size
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

        SDL_Renderer* renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_ACCELERATED | SDL_RENDERER_PRESENTVSYNC);
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

        while (running) {
            while (SDL_PollEvent(&event)) {
                if (event.type == SDL_QUIT) {
                    running = false;
                } else if (event.type == SDL_KEYDOWN || event.type == SDL_KEYUP) {
                    handle_key_event(event, current_buttons);
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

            rust::Slice<const uint8_t> video_slice = ffi::get_video_buffer(*emu);
            SDL_UpdateTexture(texture, NULL, video_slice.data(), width * 3);
            SDL_RenderClear(renderer);
            SDL_RenderCopy(renderer, texture, NULL, NULL);
            SDL_RenderPresent(renderer);

            rust::Slice<const int16_t> audio_slice = ffi::get_audio_buffer(*emu);
            if (audio_device != 0) {
                SDL_QueueAudio(audio_device, audio_slice.data(), audio_slice.size() * sizeof(int16_t));
            }

            SDL_Delay(16);
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

