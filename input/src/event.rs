mod button;
mod key;

pub use button::Button;
pub use key::Key;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    Input { device_id: u16, input: InputEvent, syn: bool },
    NewDevice(Device),
    RemoveDevice(u16),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum InputEvent {
    Key { direction: Direction, kind: KeyKind },
    Other { type_: u16, code: u16, value: i32 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: u16,
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    pub bustype: u16,
    pub version: u16,
    pub capabilities: Vec<Capability>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Capability {
    Other { type_: u16, code: u16 },
    ABS { code: u16, info: AbsInfo },
    REP { code: u16, value: i32 },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Axis {
    X,
    Y,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum Direction {
    Up,   // The key is released.
    Down, // The key is pressed.
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum KeyKind {
    Key(Key),
    Button(Button),
}
