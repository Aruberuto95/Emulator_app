#pragma once
#include "rust/cxx.h"
#include <memory>
#include <cstdint>

namespace n64 {
struct VideoFrame;
struct RendererState;
class Renderer {
public:
    explicit Renderer(rust::Slice<std::uint8_t> rdram);
    ~Renderer();
    Renderer(const Renderer&) = delete;
    Renderer& operator=(const Renderer&) = delete;
    std::uint64_t process(rust::Slice<const std::uint8_t> dmem, rust::Slice<std::uint32_t> registers);
    void set_register(std::uint32_t index, std::uint32_t value);
    VideoFrame scanout(bool render);
    void synchronize();
    RendererState capture();
    void restore(const RendererState& state);
private:
    struct Impl;
    std::unique_ptr<Impl> impl;
};
std::unique_ptr<Renderer> create_renderer(rust::Slice<std::uint8_t> rdram);
}
