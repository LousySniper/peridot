//! Types for the generated Razer model table.

pub const BATTERY: u32 = 1 << 0;
pub const CHARGING: u32 = 1 << 1;
pub const IDLE: u32 = 1 << 2;
pub const LOW_BATTERY: u32 = 1 << 3;
pub const DPI: u32 = 1 << 4;
pub const DPI_STAGES: u32 = 1 << 5;
pub const POLL: u32 = 1 << 6;
/// HyperPolling command set (up to 8 kHz).
pub const POLL2: u32 = 1 << 7;
/// Polling changes must be sent twice (argument 0 and 1).
pub const POLL2_DUAL: u32 = 1 << 8;
/// Legacy single-byte DPI command.
pub const DPI_BYTE: u32 = 1 << 9;
pub const WIRELESS: u32 = 1 << 10;

// Zone effect bits
pub const FX_OFF: u8 = 1;
pub const FX_STATIC: u8 = 2;
pub const FX_BREATH: u8 = 4;
pub const FX_SPECTRUM: u8 = 8;
pub const FX_WAVE: u8 = 16;
pub const FX_REACTIVE: u8 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedProto {
    None,
    /// Class 0x03 LED state/colour/effect commands (older mice).
    Standard,
    /// Class 0x03 id 0x0D "mouse extended" effects.
    MouseExtended,
    /// Class 0x0F extended matrix effects (most current devices).
    Extended,
}

#[derive(Clone, Copy, Debug)]
pub struct ZoneDef {
    pub led: u8,
    pub id: &'static str,
    pub name: &'static str,
    pub effects: u8,
    pub brightness: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Model {
    pub pid: u16,
    pub name: &'static str,
    pub tid: u8,
    pub led_tid: u8,
    pub poll_dual_tid: u8,
    pub iface: u8,
    pub wait_us: u32,
    pub dpi_max: u16,
    pub flags: u32,
    pub led: LedProto,
    pub zones: &'static [ZoneDef],
    pub poll_rates: &'static [u16],
}

impl Model {
    pub fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }
}
