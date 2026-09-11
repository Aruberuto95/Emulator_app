#[cxx::bridge(namespace = "n64")]
pub mod ffi {
    struct VideoFrame {
        width: u32,
        height: u32,
        pixels: Vec<u32>,
    }
    struct RendererState {
        registers: Vec<u32>,
        pending: Vec<u32>,
        hidden_ram: Vec<u8>,
        tmem: Vec<u8>,
        frame_index: u32,
        primitive_index: u32,
    }
    unsafe extern "C++" {
        include!("n64-engine/cpp/renderer.hpp");
        type Renderer;
        // The engine must keep RDRAM allocated, unmoved and exclusive until Renderer drops.
        unsafe fn create_renderer(rdram: &mut [u8]) -> Result<UniquePtr<Renderer>>;
        fn process(self: Pin<&mut Renderer>, dmem: &[u8], registers: &mut [u32]) -> Result<u64>;
        fn set_register(self: Pin<&mut Renderer>, index: u32, value: u32);
        fn scanout(self: Pin<&mut Renderer>, render: bool) -> Result<VideoFrame>;
        fn capture(self: Pin<&mut Renderer>) -> Result<RendererState>;
        fn restore(self: Pin<&mut Renderer>, state: &RendererState) -> Result<()>;
    }
}
