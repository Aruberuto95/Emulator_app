//! A single atomic cartridge file includes EEPROM, SRAM/Flash and controller
//! paks. Legacy raw cartridge saves remain readable; no memory is discarded.
use crate::MAX_BATTERY_BYTES;
use crate::{
    snapshot::validate_saves,
    storage::{SaveTypes, Saves},
    Engine,
};
use sha2::{Digest, Sha256};
const MAGIC: &[u8; 8] = b"N64SAVE1";
impl Engine {
    pub fn battery_data(&self) -> Result<Vec<u8>, String> {
        let payload =
            postcard::to_stdvec(&self.device.ui.storage.saves).map_err(|e| e.to_string())?;
        if payload.len() > MAX_BATTERY_BYTES - 40 {
            return Err("N64 battery exceeds size limit".into());
        }
        let mut data = Vec::with_capacity(40 + payload.len());
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(&Sha256::digest(&payload));
        data.extend_from_slice(&payload);
        Ok(data)
    }
    pub fn battery_dirty(&self) -> bool {
        let s = &self.device.ui.storage.saves;
        s.eeprom.written
            || s.sram.written
            || s.flash.written
            || s.mempak.written
            || s.sdcard.written
            || s.romsave.written
    }
    pub fn battery_clean(&mut self) {
        let s = &mut self.device.ui.storage.saves;
        for save in [
            &mut s.eeprom,
            &mut s.sram,
            &mut s.flash,
            &mut s.mempak,
            &mut s.sdcard,
        ] {
            save.written = false;
        }
        s.romsave.written = false;
    }
    pub fn load_battery(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut next = Saves::default();
        if bytes.len() >= 40 && bytes.len() <= MAX_BATTERY_BYTES && &bytes[..8] == MAGIC {
            if bytes[8..40] != Sha256::digest(&bytes[40..])[..] {
                return Err("Corrupt N64 battery checksum".into());
            }
            let (saved, tail) =
                postcard::take_from_bytes(&bytes[40..]).map_err(|e| e.to_string())?;
            if !tail.is_empty() {
                return Err("Trailing N64 battery data".into());
            }
            next = saved;
        } else {
            let types = &self.device.ui.storage.save_type;
            if (bytes.len() == 512 && types.contains(&SaveTypes::Eeprom4k))
                || (bytes.len() == 2048
                    && (types.contains(&SaveTypes::Eeprom4k)
                        || types.contains(&SaveTypes::Eeprom16k)))
            {
                next.eeprom.data = bytes.to_vec();
                next.eeprom.data.resize(2048, 0xff);
            } else if bytes.len() == 32768 && types.contains(&SaveTypes::Sram) {
                next.sram.data = bytes.to_vec();
            } else if bytes.len() == 131072 && types.contains(&SaveTypes::Flash) {
                next.flash.data = bytes.to_vec();
            } else {
                return Err("Invalid N64 battery format or size".into());
            }
        }
        validate_saves(&next)?;
        self.device.ui.storage.saves = next;
        self.battery_clean();
        Ok(())
    }
}
