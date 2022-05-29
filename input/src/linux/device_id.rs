use crate::linux::glue;

pub const VENDOR: u16 = 0xDEAD;
pub const PRODUCT: u16 = 0xDEAD;
pub const VERSION: u16 = 0xDEAD;
pub const BUSTYPE: u16 = glue::BUS_VIRTUAL as _;
