#include "n64-engine/cpp/renderer.hpp"
#include "n64-engine/src/bridge.rs.h"
#include "rdp_device.hpp"
#include "context.hpp"
#include <cstring>
#include <stdexcept>
#include <array>

namespace n64 {
static bool is_state_command(unsigned op) {
    return (op >= 0x2a && op <= 0x2f) || (op >= 0x37 && op <= 0x3f);
}
struct Renderer::Impl {
    // Destruction runs in reverse order: processor, device, then Vulkan instance.
    Vulkan::Context context;
    Vulkan::Device device;
    std::unique_ptr<RDP::CommandProcessor> processor;
    rust::Slice<std::uint8_t> ram;
    std::vector<std::uint32_t> commands;
    std::vector<RDP::RGBA> colors;
    std::uint32_t region = 5000;
    // Hardware state commands, followed by eight tile descriptors and sizes.
    // Replaying these restores decoded state without serializing C++ pointers,
    // padding, enum representations, or GPU caches.
    std::array<std::uint32_t, 160> state{};
    void remember(const std::uint32_t* words, unsigned op) {
        unsigned slot;
        if (is_state_command(op)) slot = op * 2;
        else if (op == 0x35) slot = 128 + ((words[1] >> 24) & 7) * 2;
        else if (op == 0x32 || op == 0x30 || op == 0x33 || op == 0x34)
            slot = 144 + ((words[1] >> 24) & 7) * 2;
        else return;
        state[slot] = slot >= 144 ? (words[0] & 0x00ffffff) | 0x32000000 : words[0];
        state[slot+1] = words[1];
    }
    explicit Impl(rust::Slice<std::uint8_t> memory) : ram(memory) {
        if (!Vulkan::Context::init_loader(nullptr) ||
            !context.init_instance_and_device(nullptr, 0, nullptr, 0))
            throw std::runtime_error("N64 requires a compatible Vulkan device");
        device.set_context(context);
        processor = std::make_unique<RDP::CommandProcessor>(device, ram.data(), 0,
            ram.size(), ram.size() / 2,
            RDP::COMMAND_PROCESSOR_FLAG_HOST_VISIBLE_HIDDEN_RDRAM_BIT |
            RDP::COMMAND_PROCESSOR_FLAG_HOST_VISIBLE_TMEM_BIT);
        if (!processor->device_is_supported())
            throw std::runtime_error("GPU does not support paraLLEl-RDP requirements");
        commands.reserve(65536);
    }
};
Renderer::Renderer(rust::Slice<std::uint8_t> ram) : impl(std::make_unique<Impl>(ram)) {}
Renderer::~Renderer() = default;
std::unique_ptr<Renderer> create_renderer(rust::Slice<std::uint8_t> ram) {
    if ((ram.size() != 0x400000 && ram.size() != 0x800000) ||
        reinterpret_cast<std::uintptr_t>(ram.data()) % 65536 != 0)
        throw std::runtime_error("Invalid N64 RDRAM allocation");
    return std::make_unique<Renderer>(ram);
}
void Renderer::synchronize() { impl->processor->idle(); }
void Renderer::set_register(std::uint32_t i, std::uint32_t v) {
    if (i < 14) impl->processor->set_vi_register(static_cast<RDP::VIRegister>(i), v);
}
std::uint64_t Renderer::process(rust::Slice<const std::uint8_t> dmem,
                              rust::Slice<std::uint32_t> regs) {
    if (regs.size() != 8 || dmem.size() < 4096)
        throw std::runtime_error("Invalid RDP register or DMEM span");
    const auto current = regs[2] & 0xfffff8u;
    const auto end = regs[1] & 0xfffff8u;
    if (end <= current) return 0;
    if ((end - current) / 4 + impl->commands.size() > 65536)
        throw std::runtime_error("RDP command buffer limit exceeded");
    for (auto address = current; address < end; address += 4) {
        std::uint32_t word;
        if (regs[3] & 1) {
            const auto a = address & 4095;
            word = (std::uint32_t(dmem[a]) << 24) | (std::uint32_t(dmem[a+1]) << 16) |
                (std::uint32_t(dmem[a+2]) << 8) | dmem[a+3];
        } else {
            if (address + 4 > impl->ram.size())
                throw std::runtime_error("RDP DMA outside RDRAM");
            std::memcpy(&word, impl->ram.data() + address, 4);
        }
        impl->commands.push_back(word);
    }
    regs[2] = regs[1];
    static constexpr auto lengths = [] {
        std::array<unsigned, 64> a{};
        for (auto& n : a) n = 2;
        a[8]=8; a[9]=12; a[10]=24; a[11]=28;
        a[12]=24; a[13]=28; a[14]=40; a[15]=44;
        a[36]=4; a[37]=4;
        return a;
    }();
    std::size_t consumed = 0;
    std::uint64_t timer = 0;
    while (consumed + 2 <= impl->commands.size()) {
        const auto* words = impl->commands.data() + consumed;
        const auto op = (words[0] >> 24) & 63;
        const auto count = lengths[op];
        if (consumed + count > impl->commands.size()) break;
        if (op >= 8) impl->processor->enqueue_command(count, words);
        impl->remember(words, op);
        if (op == 0x2d) {
            const auto x0 = ((words[0] >> 12) & 4095) >> 2, y0 = (words[0] & 4095) >> 2;
            const auto x1 = ((words[1] >> 12) & 4095) >> 2, y1 = (words[1] & 4095) >> 2;
            impl->region = x1 > x0 && y1 > y0 ? (x1-x0)*(y1-y0) : 5000;
        }
        if (op == 0x29) {
            // Complete host writes before the emulated DP interrupt exposes them to CPU/RSP.
            impl->processor->wait_for_timeline(impl->processor->signal_timeline());
            timer = impl->region;
        }
        consumed += count;
    }
    impl->commands.erase(impl->commands.begin(), impl->commands.begin() + consumed);
    return timer;
}
VideoFrame Renderer::scanout(bool render) {
    if (!render) {
        // Skipped presentation still retires GPU resources and advances VI noise.
        impl->processor->set_frame_index(impl->processor->get_frame_index()+1);
        impl->processor->begin_frame_context();
        return VideoFrame{};
    }
    unsigned width = 0, height = 0;
    impl->processor->scanout_sync(impl->colors, width, height);
    if (width > 2048 || height > 2048 || impl->colors.size() != std::size_t(width) * height)
        throw std::runtime_error("Invalid N64 scanout dimensions");
    VideoFrame result;
    result.width = width;
    result.height = height;
    result.pixels.reserve(impl->colors.size());
    for (const auto& p : impl->colors)
        result.pixels.push_back(std::uint32_t(p.r) | (std::uint32_t(p.g) << 8) |
            (std::uint32_t(p.b) << 16) | (std::uint32_t(p.a) << 24));
    impl->processor->begin_frame_context();
    return result;
}
RendererState Renderer::capture() {
    synchronize();
    RendererState result;
    for (auto word : impl->state) result.registers.push_back(word);
    for (auto word : impl->commands) result.pending.push_back(word);
    auto* hidden = static_cast<const std::uint8_t*>(impl->processor->begin_read_hidden_rdram());
    auto* tmem = static_cast<const std::uint8_t*>(impl->processor->get_tmem());
    if (!hidden || !tmem) throw std::runtime_error("Cannot map RDP snapshot memory");
    result.hidden_ram.reserve(impl->processor->get_hidden_rdram_size());
    for (std::size_t i=0;i<impl->processor->get_hidden_rdram_size();++i) result.hidden_ram.push_back(hidden[i]);
    result.tmem.reserve(4096);
    for (unsigned i=0;i<4096;++i) result.tmem.push_back(tmem[i]);
    result.frame_index = impl->processor->get_frame_index();
    result.primitive_index = impl->processor->get_primitive_index();
    return result;
}
void Renderer::restore(const RendererState& saved) {
    if (saved.registers.size()!=160 || saved.pending.size()>43 || saved.tmem.size()!=4096 ||
        saved.hidden_ram.size()!=impl->processor->get_hidden_rdram_size())
        throw std::runtime_error("Invalid RDP snapshot layout");
    // Validate all commands before submitting any work. Drawing and DMA commands
    // are forbidden here; texture RAM is restored from its own bounded image.
    for (unsigned i=0;i<160;i+=2) {
        auto w=saved.registers[i];
        if (!w && !saved.registers[i+1]) continue;
        unsigned op=(w>>24)&63;
        unsigned expected=i<128 ? i/2 : i<144 ? 0x35 : 0x32;
        if (op!=expected || (i<128 && !is_state_command(op)) ||
            (i>=128 && ((saved.registers[i+1]>>24)&7)!=(i%16)/2))
            throw std::runtime_error("Invalid RDP snapshot command");
    }
    synchronize();
    std::copy(saved.registers.begin(), saved.registers.end(), impl->state.begin());
    for (unsigned i=0;i<160;i+=2) {
        if (impl->state[i] || impl->state[i+1]) impl->processor->enqueue_command(2,impl->state.data()+i);
    }
    synchronize();
    auto* hidden=impl->processor->begin_read_hidden_rdram();
    auto* tmem=impl->processor->get_tmem();
    if (!hidden || !tmem) throw std::runtime_error("Cannot map RDP snapshot memory");
    std::memcpy(hidden,saved.hidden_ram.data(),saved.hidden_ram.size());
    std::memcpy(tmem,saved.tmem.data(),saved.tmem.size());
    impl->processor->end_write_hidden_rdram();
    impl->processor->end_write_tmem();
    impl->processor->set_frame_index(saved.frame_index);
    impl->processor->set_primitive_index(saved.primitive_index);
    impl->commands.assign(saved.pending.begin(), saved.pending.end());
    const auto* scissor=impl->state.data()+0x2d*2;
    auto x0=((scissor[0]>>12)&4095)>>2, y0=(scissor[0]&4095)>>2;
    auto x1=((scissor[1]>>12)&4095)>>2, y1=(scissor[1]&4095)>>2;
    impl->region=x1>x0 && y1>y0 ? (x1-x0)*(y1-y0) : 5000;
}
}
