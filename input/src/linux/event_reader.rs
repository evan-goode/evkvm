use crate::event::{Capability, AbsInfo, Device, InputEvent};
use crate::linux::glue;
use std::ffi;
use std::fs::{File, OpenOptions};
use std::io::Error;
use std::mem::MaybeUninit;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::str::FromStr;
use tokio::io::unix::AsyncFd;

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

        let ret = unsafe { glue::libevdev_grab(evdev, glue::libevdev_grab_mode_LIBEVDEV_GRAB) };
        if ret < 0 {
            unsafe {
                glue::libevdev_free(evdev);
            }
            return Err(Error::from_raw_os_error(-ret).into());
        }
        
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
