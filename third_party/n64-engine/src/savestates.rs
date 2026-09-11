use crate::device;
#[cfg(target_arch = "aarch64")]
use device::__m128i;
use serde::de::{Deserialize, Deserializer, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeSeq, Serializer};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

struct M128iArrayVisitor<const N: usize>;

impl<'de, const N: usize> Visitor<'de> for M128iArrayVisitor<N> {
    type Value = [__m128i; N];

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(&format!("an array of {N} 128-bit integers"))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut arr: [__m128i; N] = [device::zero_m128i(); N];
        for (index, item) in arr.iter_mut().enumerate().take(N) {
            match seq.next_element::<u128>()? {
                Some(value) => *item = unsafe { std::mem::transmute::<u128, __m128i>(value) },
                None => return Err(serde::de::Error::invalid_length(index, &self)),
            }
        }
        Ok(arr)
    }
}

pub fn deserialize_m128i_array<'de, D, const N: usize>(
    deserializer: D,
) -> Result<[__m128i; N], D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_seq(M128iArrayVisitor)
}

pub fn serialize_m128i<S>(data: &__m128i, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let bytes: u128 = unsafe { std::mem::transmute(*data) };
    bytes.serialize(serializer)
}

pub fn deserialize_m128i<'de, D>(deserializer: D) -> Result<__m128i, D::Error>
where
    D: Deserializer<'de>,
{
    let bytes = u128::deserialize(deserializer)?;
    Ok(unsafe { std::mem::transmute::<u128, __m128i>(bytes) })
}

pub fn serialize_m128i_array<S>(value: &[__m128i], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut seq = serializer.serialize_seq(Some(value.len()))?;

    for item in value {
        let bytes: u128 = unsafe { std::mem::transmute(*item) };
        seq.serialize_element(&bytes)?;
    }

    seq.end()
}

pub fn default_pak_handler() -> fn(&mut device::Device, usize, u16, usize, usize) {
    device::controller::mempak::read
}

pub fn default_instructions<const N: usize>() -> [fn(&mut device::Device, u32); N]
where
    [fn(&mut device::Device, u32); N]: Sized,
{
    [device::cop0::reserved; N]
}

pub fn default_memory_read_fast() -> Vec<fn(&device::Device, u64, device::memory::AccessSize) -> u32>
{
    vec![device::unmapped::read_mem_fast; 0x2000]
}

pub fn default_memory_read() -> Vec<fn(&mut device::Device, u64, device::memory::AccessSize) -> u32>
{
    vec![device::unmapped::read_mem; 0x2000]
}

pub fn default_memory_write() -> Vec<fn(&mut device::Device, u64, u32, u32)> {
    vec![device::unmapped::write_mem; 0x2000]
}

// RSP dispatch may select SIMD functions. Engine construction checks the host ISA.
pub fn default_rsp_instruction() -> unsafe fn(&mut device::Device, u32) {
    device::cop0::reserved
}
pub fn default_rsp_instructions<const N: usize>() -> [unsafe fn(&mut device::Device, u32); N] {
    [device::cop0::reserved; N]
}
