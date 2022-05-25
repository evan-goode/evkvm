use crate::event::{Capability, AbsInfo, Device, InputEvent};
use crate::linux::device_id;
use crate::linux::glue::{self, libevdev, libevdev_uinput};
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
    evdev: *mut libevdev,
}

impl EventReader {
    pub async fn open(path: &Path) -> Result<Self, OpenError> {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || Self::open_sync(&path))
            .await
            .map_err(|err| OpenError::Io(err.into()))?
    }

    fn open_sync(path: &Path) -> Result<Self, OpenError> {
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

        // Check if we're not opening our own virtual device. TODO
        if version == device_id::VERSION as _ {
            unsafe {
                glue::libevdev_free(evdev);
            }

            return Err(OpenError::AlreadyOpened);
        }

        let name = unsafe {
            let name_ptr = glue::libevdev_get_name(evdev);
            ffi::CStr::from_ptr(name_ptr).to_str().unwrap().to_owned()
        };

        let file_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .unwrap();
        let num_str = &file_name[String::from("event").len()..];
        let id = u16::from_str(num_str).unwrap_or(0);

        // let id = unsafe {
        //     let uniq_ptr = glue::libevdev_get_name(evdev);
        //     if uniq_ptr.is_null() {
        //         name.clone()
        //     } else {
        //         ffi::CStr::from_ptr(name_ptr).to_str().unwrap().to_owned()
        //     }
        // };

        let mut capabilities = Vec::new();
        for type_ in 0..glue::EV_MAX {
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
            // name,
            vendor: vendor as u16,
            product: product as u16,
            bustype: bustype as u16,
            version: version as u16,
            capabilities,
        };

        // unsafe {
        //     glue::libevdev_set_id_vendor(evdev, device_id::VENDOR as _);
        //     glue::libevdev_set_id_product(evdev, device_id::PRODUCT as _);
        //     glue::libevdev_set_id_version(evdev, device_id::VERSION as _);
        // }

        // Don't grab for now
        // let ret = unsafe { glue::libevdev_grab(evdev, glue::libevdev_grab_mode_LIBEVDEV_GRAB) };
        // if ret < 0 {
        //     unsafe {
        //         glue::libevdev_free(evdev);
        //     }

        //     return Err(Error::from_raw_os_error(-ret).into());
        // }

        // let mut uinput = MaybeUninit::uninit();
        // let ret = unsafe {
        //     glue::libevdev_uinput_create_from_device(
        //         evdev,
        //         glue::libevdev_uinput_open_mode_LIBEVDEV_UINPUT_OPEN_MANAGED,
        //         uinput.as_mut_ptr(),
        //     )
        // };

        // if ret < 0 {
        //     unsafe { glue::libevdev_free(evdev) };
        //     return Err(Error::from_raw_os_error(-ret).into());
        // }

        // let uinput = unsafe { uinput.assume_init() };
        //
        Ok(Self {
            file,
            evdev,
            device,
            // uinput,
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

            // Event::from_raw(event);
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
