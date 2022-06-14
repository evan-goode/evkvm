use crate::event::{Event, Capability, AbsInfo, Device, InputEvent};
use std::ffi;
use std::fs::{File, OpenOptions};
use std::mem::MaybeUninit;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::str::FromStr;
use tokio::io::unix::AsyncFd;
use crate::linux::glue;
use futures::StreamExt;
use inotify::{Inotify, WatchMask};
use std::io::{Error, ErrorKind};
use std::path::Path;
use std::time::Duration;
use std::collections::HashMap;
use tokio::fs;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::sync::oneshot;
use tokio::time;

const EVENT_PATH: &str = "/dev/input";

pub(crate) struct EventReader {
    pub device: Device,
    file: AsyncFd<File>,
    evdev: *mut glue::libevdev,
}

impl EventReader {
    pub fn new(path: &Path) -> Result<Self, OpenError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .and_then(AsyncFd::new)?;

        let mut evdev = MaybeUninit::uninit();
        let ret = unsafe { glue::libevdev_new_from_fd(file.as_raw_fd(), evdev.as_mut_ptr()) };
        if ret < 0 {
            return Err(Error::from_raw_os_error(-ret).into());
        }

        let evdev = unsafe { evdev.assume_init() };
        let (product, vendor, bustype, version) = unsafe {
            (
                glue::libevdev_get_id_product(evdev),
                glue::libevdev_get_id_vendor(evdev),
                glue::libevdev_get_id_bustype(evdev),
                glue::libevdev_get_id_version(evdev),
            )
        };

        let name_c_str = unsafe {
            let name_buf = glue::libevdev_get_name(evdev);
            ffi::CStr::from_ptr(name_buf)
        };
        let name = name_c_str.to_str().unwrap().to_owned();

        if (bustype as u32) == glue::BUS_VIRTUAL {
            unsafe {
                glue::libevdev_free(evdev);
            }

            return Err(OpenError::AlreadyOpened);
        }

        let file_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .unwrap();
        let num_str = &file_name[String::from("event").len()..];
        let id = u16::from_str(num_str).unwrap_or(0);

        let mut capabilities = Vec::new();
        for type_ in 0..glue::EV_MAX {
            if type_ == glue::EV_SW { continue; } // ignore EV_SW for now
            let has_type = unsafe {
                glue::libevdev_has_event_type(evdev, type_)
            } == 1;
            if !has_type { continue; }
            let code_max = unsafe {
                glue::libevdev_event_type_get_max(type_)
            } as u32;
            for code in 0..code_max {
                let has_code = unsafe {
                    glue::libevdev_has_event_code(evdev, type_, code)
                } == 1;
                if has_code {
                    let capability = match type_ {
                        glue::EV_ABS => {
                            let info = unsafe {
                                let raw_info = glue::libevdev_get_abs_info(evdev, code);
                                if raw_info.is_null() {
                                    panic!("absinfo is null!");
                                }
                                AbsInfo {
                                    value: (*raw_info).value,
                                    minimum: (*raw_info).minimum,
                                    maximum: (*raw_info).maximum,
                                    fuzz: (*raw_info).fuzz,
                                    flat: (*raw_info).flat,
                                    resolution: (*raw_info).resolution,
                                }
                            };
                            Capability::ABS { code: code as u16, info }
                        },
                        glue::EV_REP => {
                            let value = unsafe {
                                glue::libevdev_get_event_value(evdev, type_, code)
                            };
                            Capability::REP { code: code as u16, value }
                        },
                        _ => Capability::Other {
                            type_: type_ as u16,
                            code: code as u16
                        },
                    };
                    capabilities.push(capability);
                }
            }
        }

        let device = Device {
            id,
            name,
            vendor: vendor as u16,
            product: product as u16,
            bustype: bustype as u16,
            version: version as u16,
            capabilities,
        };

        // let ret = unsafe { glue::libevdev_grab(evdev, glue::libevdev_grab_mode_LIBEVDEV_GRAB) };
        // if ret < 0 {
        //     unsafe {
        //         glue::libevdev_free(evdev);
        //     }
        //     return Err(Error::from_raw_os_error(-ret).into());
        // }
        
        Ok(Self {
            file,
            evdev,
            device,
        })
    }

    pub async fn read(&mut self) -> Result<InputEvent, Error> {
        loop {
            let result = self.file.readable().await?.try_io(|_| {
                let mut event = MaybeUninit::uninit();
                let ret = unsafe {
                    glue::libevdev_next_event(
                        self.evdev,
                        glue::libevdev_read_flag_LIBEVDEV_READ_FLAG_NORMAL,
                        event.as_mut_ptr(),
                    )
                };

                if ret < 0 {
                    return Err(Error::from_raw_os_error(-ret));
                }

                let event = unsafe { event.assume_init() };
                Ok(event)
            });

            let event = match result {
                Ok(Ok(event)) => event,
                Ok(Err(err)) => return Err(err),
                Err(_) => continue, // This means it would block.
            };

            if let Some(event) = InputEvent::from_raw(event) {
                return Ok(event);
            }
        }
    }
}

impl Drop for EventReader {
    fn drop(&mut self) {
        unsafe {
            glue::libevdev_free(self.evdev);
        }
    }
}

unsafe impl Send for EventReader {}

pub enum OpenError {
    AlreadyOpened,
    Io(Error),
}

impl From<Error> for OpenError {
    fn from(err: Error) -> Self {
        OpenError::Io(err)
    }
}

pub struct ReaderManager {
    pub devices: HashMap<u16, Device>,
    event_receiver: mpsc::UnboundedReceiver<Result<Event, Error>>,
    watcher_receiver: oneshot::Receiver<Error>,
}

impl ReaderManager {
    pub async fn new() -> Result<Self, Error> {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        // HACK: When rkvm is run from the terminal, a race condition happens where the enter key
        // release event is swallowed and the key will remain in a "pressed" state until the user manually presses it again.
        // This is presumably due to the event being generated while we're in the process of grabbing
        // the keyboard input device.
        //
        // This won't prevent this from happenning with other keys if they happen to be pressed at an
        // unfortunate time, but that is unlikely to happen and will ease the life of people who run rkvm
        // directly from the terminal for the time being until a proper fix is made.
        time::sleep(Duration::from_millis(500)).await;

        let devices: HashMap<u16, Device> = HashMap::new();

        let mut read_dir = fs::read_dir(EVENT_PATH).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            spawn_reader(&entry.path(), event_sender.clone()).await?;
        }

        let (watcher_sender, watcher_receiver) = oneshot::channel();
        tokio::spawn(async {
            if let Err(err) = handle_notify(event_sender).await {
                let _ = watcher_sender.send(err);
            }
        });

        Ok(ReaderManager {
            devices,
            event_receiver,
            watcher_receiver,
        })
    }

    pub async fn read(&mut self) -> Result<Event, Error> {
        if let Ok(err) = self.watcher_receiver.try_recv() {
            return Err(err);
        }

        let event_result = self.event_receiver
            .recv()
            .await
            .ok_or_else(|| Error::new(ErrorKind::Other, "All devices closed"))?;

        match event_result {
            Ok(Event::NewDevice(ref device)) => {
                self.devices.insert(device.id, device.clone());
            },
            Ok(Event::RemoveDevice(device_id)) => {
                self.devices.remove(&device_id);
            },
            _ => {},
        }

        event_result
    }
}

async fn spawn_reader(
    path: &Path,
    event_sender: mpsc::UnboundedSender<Result<Event, Error>>,
) -> Result<(), Error> {
    if path.is_dir() {
        return Ok(());
    }

    // Skip non input event files.
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| !name.starts_with("event"))
        .unwrap_or(true)
    {
        return Ok(());
    }

    let reader = match EventReader::new(&path) {
        Ok(reader) => reader,
        Err(OpenError::Io(err)) => return Err(err),
        Err(OpenError::AlreadyOpened) => return Ok(()),
    };

    let event = Event::NewDevice(reader.device.clone());
    event_sender.send(Ok(event)).unwrap();

    tokio::spawn(handle_events(reader, event_sender));

    Ok(())
}

async fn handle_notify(
    sender: mpsc::UnboundedSender<Result<Event, Error>>,
) -> Result<(), Error> {
    let mut inotify = Inotify::init()?;
    inotify.add_watch(EVENT_PATH, WatchMask::CREATE)?;

    // This buffer size should be OK, since we don't expect a lot of devices
    // to be plugged in frequently.
    let mut stream = inotify.event_stream([0u8; 512])?;
    while let Some(event) = stream.next().await {
        let event = event?;

        if let Some(name) = event.name {
            let path = Path::new(EVENT_PATH).join(&name);
            spawn_reader(&path, sender.clone()).await?;
        }
    }

    Ok(())
}

async fn handle_events(
    mut reader: EventReader,
    sender: mpsc::UnboundedSender<Result<Event, Error>>,
) -> Result<(), watch::error::RecvError> {
    loop {
        let result = match reader.read().await {
            Ok(input_event) => {
                let event = Event::Input {
                    device_id: reader.device.id,
                    input: input_event,
                    syn: false,
                };
                sender.send(Ok(event)).is_ok()
            }
            // This happens if the device is disconnected.
            // In that case simply terminate the reading task.
            Err(ref err) if err.raw_os_error() == Some(libc::ENODEV) => {
                let event = Event::RemoveDevice(reader.device.id);
                let _ = sender.send(Ok(event));
                false
            },
            Err(err) => {
                let _ = sender.send(Err(err));
                false
            },
        };

        if !result {
            return Ok(());
        }
    }
}
