mod event;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{ReaderManager, WriterManager};

pub use event::{Axis, Button, Direction, Event, InputEvent, Device, Key, KeyKind};
