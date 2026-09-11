//! Embedded N64 emulation; see UPSTREAM.md for provenance and adapter decisions.
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_BATTERY_BYTES: usize = 70 * 1024 * 1024;
/// Host preferences are separate from snapshots so loading a game cannot replace
/// the player's current controller setup. Auto retains cartridge compatibility.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum Accessory {
    #[default]
    Auto,
    None,
    ControllerPak,
    RumblePak,
}
#[cfg(target_arch = "x86_64")]
include!("machine.rs");
#[cfg(not(target_arch = "x86_64"))]
mod unsupported;
#[cfg(not(target_arch = "x86_64"))]
pub use unsupported::Engine;
