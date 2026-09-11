//! Versioned, cartridge-bound snapshots. Validate a separate machine and native
//! renderer before swapping it in, so a corrupt slot cannot damage a live game.
use crate::{bridge::ffi, device, guest_boundary, storage, Engine};
use sha2::{Digest, Sha256};

use crate::MAX_STATE_BYTES;
const MAGIC: &[u8; 8] = b"N64STATE";
const HEADER: usize = 80;

// cxx's generated shared structs cannot derive serde. Keep their field ordering
// in one declaration for both directions, with no intermediate pixel copies.
macro_rules! shared_serde {
    ($ty:ty, $($field:ident: $kind:ty),+ $(,)?) => {
        impl serde::Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, s:S) -> Result<S::Ok,S::Error> {
                serde::Serialize::serialize(&($(&self.$field,)+),s)
            }
        }
        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d:D) -> Result<Self,D::Error> {
                let ($($field,)+): ($($kind,)+) = serde::Deserialize::deserialize(d)?;
                Ok(Self { $($field,)+ })
            }
        }
    }
}
shared_serde!(ffi::VideoFrame, width:u32, height:u32, pixels:Vec<u32>);
shared_serde!(ffi::RendererState, registers:Vec<u32>, pending:Vec<u32>, hidden_ram:Vec<u8>, tmem:Vec<u8>, frame_index:u32, primitive_index:u32);

#[derive(serde::Serialize, serde::Deserialize)]
struct AudioState {
    input_hz: u64,
    phase: f64,
    previous: [i16; 2],
    controllers: [bool; 4],
    input: [u32; 4],
}
type State = (
    Box<device::Device>,
    storage::Saves,
    ffi::RendererState,
    AudioState,
    ffi::VideoFrame,
);

impl Engine {
    pub fn save_state(&mut self) -> Result<Vec<u8>, String> {
        guest_boundary(|| {
            // Flush GPU writes before reading CPU-visible RDRAM for serialization.
            let graphics = self
                .device
                .ui
                .renderer
                .pin_mut()
                .capture()
                .map_err(|e| e.to_string())?;
            let ui = &self.device.ui;
            let audio = AudioState {
                input_hz: ui.input_hz,
                phase: ui.phase,
                previous: ui.previous,
                controllers: ui.controllers,
                input: ui.input,
            };
            let payload = postcard::to_stdvec(&(
                &self.device,
                &ui.storage.saves,
                &graphics,
                &audio,
                &self.frame,
            ))
            .map_err(|e| e.to_string())?;
            if payload.len() > MAX_STATE_BYTES - HEADER {
                return Err("N64 state exceeds size limit".into());
            }
            let mut output = Vec::with_capacity(HEADER + payload.len());
            output.extend_from_slice(MAGIC);
            output.extend_from_slice(&1u32.to_le_bytes());
            output.extend_from_slice(&Sha256::digest(&self.device.cart.rom));
            output.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            output.extend_from_slice(&Sha256::digest(&payload));
            output.extend_from_slice(&payload);
            Ok(output)
        })
    }

    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        guest_boundary(|| self.restore_snapshot(bytes))
    }

    fn restore_snapshot(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() < HEADER
            || bytes.len() > MAX_STATE_BYTES
            || &bytes[..8] != MAGIC
            || bytes[8..12] != 1u32.to_le_bytes()
        {
            return Err("Invalid N64 snapshot header or version".into());
        }
        let payload = &bytes[HEADER..];
        if bytes[12..44] != Sha256::digest(&self.device.cart.rom)[..]
            || bytes[44..48] != (payload.len() as u32).to_le_bytes()
            || bytes[48..80] != Sha256::digest(payload)[..]
        {
            return Err("N64 snapshot is corrupt or belongs to another ROM".into());
        }
        let ((mut d, saves, graphics, audio, frame), remaining): (State, _) =
            postcard::take_from_bytes(payload).map_err(|e| format!("Invalid N64 snapshot: {e}"))?;
        if !remaining.is_empty() {
            return Err("Trailing N64 snapshot data".into());
        }
        if d.rdram.size as usize != d.rdram.mem.len()
            || d.rdram.size != self.device.rdram.size
            || d.memory.icache.len() != 512
            || d.memory.dcache.len() != 512
            || d.cpu.cop0.tlb_lut_r.len() != 0x100000
            || d.cpu.cop0.tlb_lut_w.len() != 0x100000
            || d.cart.is_viewer_buffer.len() != 0xffff
            || d.cart.sc64.buffer.len() != 8192
            || d.cart.sc64.writeback_sector.len() != 256
            || d.cpu.next_event >= device::events::EVENT_TYPE_COUNT
            || d.rsp.regs2[0] > 4095
            || d.rsp.regs2[0] % 4 != 0
            || d.byte_swap != self.device.byte_swap
            || d.cpu.clock_rate != self.device.cpu.clock_rate
            || !audio.phase.is_finite()
            || !(0.0..=50_000_000.0).contains(&audio.phase)
            || !(1..=50_000_000).contains(&audio.input_hz)
            || frame.width == 0
            || frame.height == 0
            || frame.width > 2048
            || frame.height > 2048
            || frame.pixels.len() != frame.width as usize * frame.height as usize
        {
            return Err("Invalid N64 snapshot machine layout".into());
        }
        for channel in &d.pif.channels {
            for offset in [channel.tx, channel.tx_buf, channel.rx, channel.rx_buf]
                .into_iter()
                .flatten()
            {
                if offset >= 64 {
                    return Err("Invalid PIF snapshot offset".into());
                }
            }
            if channel
                .pak_handler
                .is_some_and(|p| p.pak_type == device::controller::PakType::TransferPak)
            {
                return Err("Unsupported snapshot controller accessory".into());
            }
        }
        validate_saves(&saves)?;
        d.cart.rom = self.device.cart.rom.clone();
        d.ui.output_hz = self.device.ui.output_hz;
        d.ui.input_hz = audio.input_hz;
        d.ui.phase = audio.phase;
        d.ui.previous = audio.previous;
        d.ui.controllers = audio.controllers;
        d.ui.input = audio.input;
        d.ui.game_id = self.device.ui.game_id.clone();
        d.ui.game_hash = self.device.ui.game_hash.clone();
        d.ui.storage.save_type = storage::get_save_type(&d.cart.rom, &d.ui.game_id);
        d.ui.storage.saves = saves;
        device::memory::init(&mut d);
        device::cpu::map_instructions(&mut d);
        device::cop0::map_instructions(&mut d);
        device::cop1::map_instructions(&mut d);
        device::cop2::map_instructions(&mut d);
        device::rsp_cpu::map_instructions(&mut d);
        for index in 0..1024 {
            let offset = 4096 + index * 4;
            let opcode = u32::from_be_bytes(d.rsp.mem[offset..offset + 4].try_into().unwrap());
            d.rsp.cpu.instructions[index] = device::rsp_cpu::Instructions {
                func: device::rsp_cpu::decode_opcode(&d, opcode),
                opcode,
            };
        }
        for line in 0..512 {
            for word in 0..8 {
                d.memory.icache[line].instruction[word] =
                    device::cpu::decode_opcode(&d, d.memory.icache[line].words[word]);
            }
        }
        device::pif::connect_pif_channels(&mut d);
        for channel in &mut d.pif.channels[..4] {
            if let Some(handler) = &mut channel.pak_handler {
                use device::controller::{mempak, rumble, PakType};
                if handler.pak_type == PakType::RumblePak {
                    handler.read = rumble::read;
                    handler.write = rumble::write;
                } else {
                    handler.read = mempak::read;
                    handler.write = mempak::write;
                }
            }
        }
        // SAFETY: the candidate owns this aligned allocation, and its Ui drops
        // the renderer before RDRAM. No live pointers are deserialized.
        d.ui.renderer =
            unsafe { ffi::create_renderer(&mut d.rdram.mem) }.map_err(|e| e.to_string())?;
        d.ui.renderer
            .pin_mut()
            .restore(&graphics)
            .map_err(|e| e.to_string())?;
        for (index, value) in d.vi.regs.iter().enumerate() {
            d.ui.renderer.pin_mut().set_register(index as u32, *value);
        }
        let mut next = Self { device: d, frame, accessories: self.accessories };
        next.apply_accessories();
        next.mark_battery_dirty();
        *self = next;
        Ok(())
    }

    pub(crate) fn mark_battery_dirty(&mut self) {
        let saves = &mut self.device.ui.storage.saves;
        for save in [
            &mut saves.eeprom,
            &mut saves.sram,
            &mut saves.flash,
            &mut saves.mempak,
            &mut saves.sdcard,
        ] {
            save.written = !save.data.is_empty();
        }
        saves.romsave.written = !saves.romsave.data.is_empty();
    }
}

pub(crate) fn validate_saves(s: &storage::Saves) -> Result<(), String> {
    for (len, max) in [
        (s.eeprom.data.len(), 2048),
        (s.sram.data.len(), 32768),
        (s.flash.data.len(), 131072),
        (s.mempak.data.len(), 131072),
        (s.sdcard.data.len(), 64 * 1024 * 1024),
        (s.romsave.data.len(), 64 * 1024 * 1024),
    ] {
        if len > max {
            return Err("Invalid N64 save-memory size".into());
        }
    }
    Ok(())
}
