pub mod button;
pub mod key;

use crate::event::{Button, Direction, InputEvent, Key, KeyKind};
use crate::linux::glue;

impl InputEvent {
    pub(crate) fn to_raw(&self) -> glue::input_event {
        let (type_, code, value) = match *self {
            InputEvent::Other {
                type_,
                code,
                value,
            } => (type_, code, value),
            InputEvent::Key {
                direction: Direction::Up,
                kind,
            } => (glue::EV_KEY as _, kind.to_raw(), 0),
            InputEvent::Key {
                direction: Direction::Down,
                kind,
            } => (glue::EV_KEY as _, kind.to_raw(), 1),
        };

        glue::input_event {
            type_,
            code,
            value,
            time: glue::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
        }
    }

    pub(crate) fn from_raw(raw: glue::input_event) -> Option<Self> {
        let event = match (raw.type_ as _, raw.code as _, raw.value) {
            (glue::EV_KEY, code, 0) => InputEvent::Key {
                direction: Direction::Up,
                kind: KeyKind::from_raw(code as _)?,
            },
            (glue::EV_KEY, code, 1) => InputEvent::Key {
                direction: Direction::Down,
                kind: KeyKind::from_raw(code as _)?,
            },
            (type_, code, value) => InputEvent::Other {
                type_: type_ as _,
                code,
                value,
            },
        };

        Some(event)
    }
}

impl KeyKind {
    pub(crate) fn from_raw(code: u16) -> Option<KeyKind> {
        Key::from_raw(code)
            .map(KeyKind::Key)
            .or_else(|| Button::from_raw(code).map(KeyKind::Button))
    }

    pub(crate) fn to_raw(&self) -> u16 {
        match self {
            KeyKind::Key(key) => key.to_raw(),
            KeyKind::Button(button) => button.to_raw(),
        }
    }
}
