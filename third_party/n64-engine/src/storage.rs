#[derive(PartialEq)]
pub enum SaveTypes {
    Eeprom4k,
    Eeprom16k,
    Sram,
    Flash,
}

// the bool indicates whether the save has been written to
// if that is the case, it will be flushed to the disk when the program closes
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Save {
    pub data: Vec<u8>,
    pub written: bool,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct RomSave {
    pub data: std::collections::HashMap<u32, u8>,
    pub written: bool,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Saves {
    pub eeprom: Save,
    pub sram: Save,
    pub flash: Save,
    pub mempak: Save,
    pub sdcard: Save,
    pub romsave: RomSave,
    pub write_to_disk: bool,
}

pub fn get_save_type(rom: &[u8], game_id: &str) -> Vec<SaveTypes> {
    let header_type = std::str::from_utf8(rom[0x3C..0x3E].try_into().unwrap());
    if header_type.is_ok() && header_type.unwrap() == "ED" {
        let save_type = rom[0x3F] >> 4;
        match save_type {
            0 => return vec![],
            1 => return vec![SaveTypes::Eeprom4k],
            2 => return vec![SaveTypes::Eeprom16k],
            3 => return vec![SaveTypes::Sram],
            4 => panic!("Unsupported save type: {save_type}"),
            5 => return vec![SaveTypes::Flash],
            6 => panic!("Unsupported save type: {save_type}"),
            _ => panic!("Unknown save type: {save_type}"),
        }
    }
    match game_id {
        "NB7" | // Banjo-Tooie [Banjo to Kazooie no Daiboken 2 (J)]
        "NGT" | // City Tour GrandPrix - Zen Nihon GT Senshuken
        "NFU" | // Conker's Bad Fur Day
        "NCW" | // Cruis'n World
        "NCZ" | // Custom Robo V2
        "ND6" | // Densha de Go! 64
        "NDO" | // Donkey Kong 64
        "ND2" | // Doraemon 2: Nobita to Hikari no Shinden
        "N3D" | // Doraemon 3: Nobita no Machi SOS!
        "NMX" | // Excitebike 64
        "NGC" | // GT 64: Championship Edition
        "NIM" | // Ide Yosuke no Mahjong Juku
        "NNB" | // Kobe Bryant in NBA Courtside
        "NMV" | // Mario Party 3
        "NM8" | // Mario Tennis
        "NEV" | // Neon Genesis Evangelion
        "NPP" | // Parlor! Pro 64: Pachinko Jikki Simulation Game
        "NUB" | // PD Ultraman Battle Collection 64
        "NPD" | // Perfect Dark
        "NRZ" | // Ridge Racer 64
        "NR7" | // Robot Poncots 64: 7tsu no Umi no Caramel
        "NEP" | // Star Wars Episode I: Racer
        "NYS"   // Yoshi's Story
        => {
            vec![SaveTypes::Eeprom16k]
        }
        "NCC" | // Command & Conquer
        "NDA" | // Derby Stallion 64
        "NAF" | // Doubutsu no Mori
        "NJF" | // Jet Force Gemini [Star Twins (J)]
        "NKJ" | // Ken Griffey Jr.'s Slugfest
        "NZS" | // Legend of Zelda: Majora's Mask [Zelda no Densetsu - Mujura no Kamen (J)]
        "NM6" | // Mega Man 64
        "NCK" | // NBA Courtside 2 featuring Kobe Bryant
        "NMQ" | // Paper Mario
        "NPN" | // Pokemon Puzzle League
        "NPF" | // Pokemon Snap [Pocket Monsters Snap (J)]
        "NPO" | // Pokemon Stadium
        "CP2" | // Pocket Monsters Stadium 2 (J)
        "NP3" | // Pokemon Stadium 2 [Pocket Monsters Stadium - Kin Gin (J)]
        "NRH" | // Rockman Dash - Hagane no Boukenshin (J)
        "NSQ" | // StarCraft 64
        "NT9" | // Tigger's Honey Hunt
        "NW4" | // WWF No Mercy
        "NDP"   // Dinosaur Planet (Unlicensed)
        =>{
            vec![SaveTypes::Flash]
        }
        "NPQ" // Powerpuff Girls: Chemical X Traction
        => {vec![]}
        _ => {
            vec![SaveTypes::Eeprom4k, SaveTypes::Sram]
        }
    }
}
