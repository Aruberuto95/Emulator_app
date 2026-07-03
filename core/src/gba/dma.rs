#[derive(Clone, Copy, Debug)]
pub struct DmaChannel {
    pub sad: u32,
    pub dad: u32,
    pub count: u32,
    pub control: u16,

    // Internal registers
    pub cur_src: u32,
    pub cur_dest: u32,
    pub cur_count: u32,
    pub active: bool,
}

impl DmaChannel {
    pub fn new() -> Self {
        Self {
            sad: 0,
            dad: 0,
            count: 0,
            control: 0,
            cur_src: 0,
            cur_dest: 0,
            cur_count: 0,
            active: false,
        }
    }

    pub fn write_sad(&mut self, byte_offset: u32, value: u8) {
        let shift = (byte_offset & 3) * 8;
        let mask = !(0xFF << shift);
        self.sad = (self.sad & mask) | ((value as u32) << shift);
    }

    pub fn write_dad(&mut self, byte_offset: u32, value: u8) {
        let shift = (byte_offset & 3) * 8;
        let mask = !(0xFF << shift);
        self.dad = (self.dad & mask) | ((value as u32) << shift);
    }

    pub fn write_count(&mut self, byte_offset: u32, value: u8) {
        let shift = (byte_offset & 3) * 8;
        let mask = !(0xFF << shift);
        self.count = (self.count & mask) | ((value as u32) << shift);
    }

    pub fn write_control(&mut self, byte_offset: u32, value: u8) {
        let shift = (byte_offset & 1) * 8;
        let mask = !(0xFF << shift);
        let new_ctrl = (self.control & mask) | ((value as u16) << shift);

        let was_enabled = (self.control & 0x8000) != 0;
        let is_enabled = (new_ctrl & 0x8000) != 0;

        self.control = new_ctrl;

        if is_enabled && !was_enabled {
            // Enable DMA: reload registers
            self.cur_src = self.sad;
            self.cur_dest = self.dad;
            self.cur_count = if self.count == 0 {
                // Channel 3 supports 16-bit count, others 14-bit
                0x4000 // default or max count
            } else {
                self.count
            };
            self.active = true;
        } else if !is_enabled {
            self.active = false;
        }
    }
}

pub struct GbaDma {
    pub channels: [DmaChannel; 4],
}

impl GbaDma {
    pub fn new() -> Self {
        Self {
            channels: [
                DmaChannel::new(),
                DmaChannel::new(),
                DmaChannel::new(),
                DmaChannel::new(),
            ],
        }
    }
}
