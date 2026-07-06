/// RDRAM (8MB memory space) for the Nintendo 64 Core.
pub struct Rdram {
    pub data: Box<[u8; Self::ALLOC_SIZE]>,
}

impl Rdram {
    pub const SIZE: usize = 8 * 1024 * 1024; // 8MB
    pub const ALLOC_SIZE: usize = Self::SIZE + 8; // Padded size
    pub const MASK: u32 = 0x007F_FFFF; // Address mask for 8MB range

    pub fn new() -> Self {
        Self {
            data: Box::new([0; Self::ALLOC_SIZE]),
        }
    }

    #[inline(always)]
    pub fn read_u8(&self, addr: u32) -> u8 {
        let offset = (addr & Self::MASK) as usize;
        self.data[offset]
    }

    #[inline(always)]
    pub fn write_u8(&mut self, addr: u32, val: u8) {
        let offset = (addr & Self::MASK) as usize;
        self.data[offset] = val;
    }

    #[inline(always)]
    pub fn read_u16(&self, addr: u32) -> u16 {
        let offset = (addr & Self::MASK) as usize;
        let bytes = [
            self.data[offset],
            self.data[offset + 1],
        ];
        u16::from_be_bytes(bytes)
    }

    #[inline(always)]
    pub fn write_u16(&mut self, addr: u32, val: u16) {
        let offset = (addr & Self::MASK) as usize;
        let bytes = val.to_be_bytes();
        self.data[offset] = bytes[0];
        self.data[offset + 1] = bytes[1];
    }

    #[inline(always)]
    pub fn read_u32(&self, addr: u32) -> u32 {
        let offset = (addr & Self::MASK) as usize;
        let bytes = [
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
        ];
        u32::from_be_bytes(bytes)
    }

    #[inline(always)]
    pub fn write_u32(&mut self, addr: u32, val: u32) {
        let offset = (addr & Self::MASK) as usize;
        let bytes = val.to_be_bytes();
        self.data[offset] = bytes[0];
        self.data[offset + 1] = bytes[1];
        self.data[offset + 2] = bytes[2];
        self.data[offset + 3] = bytes[3];
    }

    #[inline(always)]
    pub fn read_u64(&self, addr: u32) -> u64 {
        let offset = (addr & Self::MASK) as usize;
        let bytes = [
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
            self.data[offset + 4],
            self.data[offset + 5],
            self.data[offset + 6],
            self.data[offset + 7],
        ];
        u64::from_be_bytes(bytes)
    }

    #[inline(always)]
    pub fn write_u64(&mut self, addr: u32, val: u64) {
        let offset = (addr & Self::MASK) as usize;
        let bytes = val.to_be_bytes();
        self.data[offset] = bytes[0];
        self.data[offset + 1] = bytes[1];
        self.data[offset + 2] = bytes[2];
        self.data[offset + 3] = bytes[3];
        self.data[offset + 4] = bytes[4];
        self.data[offset + 5] = bytes[5];
        self.data[offset + 6] = bytes[6];
        self.data[offset + 7] = bytes[7];
    }
}
