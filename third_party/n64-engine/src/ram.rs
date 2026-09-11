//! RDRAM owns its allocation; Vulkan borrows the aligned sub-slice.
//! Overallocating a byte Vec keeps allocation and deallocation layouts identical.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ops::{Deref, DerefMut};

#[derive(Default)]
pub struct AlignedRam {
    allocation: Vec<u8>,
    offset: usize,
    length: usize,
}
impl AlignedRam {
    pub fn new(length: usize) -> Self {
        assert!(length <= 8 * 1024 * 1024);
        let allocation = vec![0; length + 65535];
        let offset = allocation.as_ptr().align_offset(65536);
        Self {
            allocation,
            offset,
            length,
        }
    }
}
impl Deref for AlignedRam {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.allocation[self.offset..self.offset + self.length]
    }
}
impl DerefMut for AlignedRam {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.allocation[self.offset..self.offset + self.length]
    }
}
impl Serialize for AlignedRam {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(s)
    }
}
impl<'de> Deserialize<'de> for AlignedRam {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = Vec::<u8>::deserialize(d)?;
        if !matches!(bytes.len(), 0x400000 | 0x800000) {
            return Err(serde::de::Error::custom("invalid RDRAM size"));
        }
        let mut ram = Self::new(bytes.len());
        ram.copy_from_slice(&bytes);
        Ok(ram)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn alignment_survives_move_and_drop() {
        for size in [0x400000, 0x800000] {
            let mut ram = super::AlignedRam::new(size);
            assert_eq!(ram.as_ptr() as usize % 65536, 0);
            ram[size - 1] = 17;
            let moved = Box::new(ram);
            assert_eq!(moved[size - 1], 17);
        }
    }
}
