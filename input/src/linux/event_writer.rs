use crate::event::{Device, InputEvent, Capability};
use crate::linux::glue::{self, input_event, libevdev, libevdev_uinput};
use std::io::{Error, ErrorKind};
use std::mem::MaybeUninit;
use std::ffi;

pub struct EventWriter {
    evdev: *mut libevdev,
    uinput: *mut libevdev_uinput,
}

impl EventWriter {
    pub async fn new(device: Device) -> Result<Self, Error> {
        tokio::task::spawn_blocking(move || Self::new_sync(&device)).await?
    }

    fn new_sync(device: &Device) -> Result<Self, Error> {
        let evdev = unsafe { glue::libevdev_new() };
        if evdev.is_null() {
            return Err(Error::new(ErrorKind::Other, "Failed to create device"));
        }

        if let Err(err) = unsafe { setup_evdev(evdev, &device) } {
            unsafe {
                glue::libevdev_free(evdev);
            }

            return Err(err);
        }

        let mut uinput = MaybeUninit::uninit();
        let ret = unsafe {
            glue::libevdev_uinput_create_from_device(
                evdev,
                glue::libevdev_uinput_open_mode_LIBEVDEV_UINPUT_OPEN_MANAGED,
                uinput.as_mut_ptr(),
            )
        };

        if ret < 0 {
            unsafe { glue::libevdev_free(evdev) };
            return Err(Error::from_raw_os_error(-ret));
        }

        let uinput = unsafe { uinput.assume_init() };
        Ok(Self { evdev, uinput })
    }

    pub async fn write(&mut self, event: InputEvent) -> Result<(), Error> {
        self.write_raw(event.to_raw())
    }

    pub(crate) fn write_raw(&mut self, event: input_event) -> Result<(), Error> {
        // As far as tokio is concerned, the FD never becomes ready for writing, so just write it normally.
        // If an error happens, it will be propagated to caller and the FD is opened in nonblocking mode anyway,
        // so it shouldn't be an issue.
        let ret = unsafe {
            glue::libevdev_uinput_write_event(
                self.uinput as *const _,
                event.type_ as _,
                event.code as _,
                event.value,
            )
        };

        if ret < 0 {
            return Err(Error::from_raw_os_error(-ret));
        }

        Ok(())
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        unsafe {
            glue::libevdev_uinput_destroy(self.uinput);
            glue::libevdev_free(self.evdev);
        }
    }
}

unsafe impl Send for EventWriter {}

unsafe fn setup_evdev(evdev: *mut libevdev, device: &Device) -> Result<(), Error> {
    // TODO name

    glue::libevdev_set_id_vendor(evdev, device.vendor as _);
    glue::libevdev_set_id_product(evdev, device.product as _);
    glue::libevdev_set_id_version(evdev, device.version as _);
    glue::libevdev_set_id_bustype(evdev, glue::BUS_VIRTUAL as _);

    // let name = format!("skvm-{}", device.name);
    let name_c_string = ffi::CString::new(device.name.clone()).unwrap();
    glue::libevdev_set_name(evdev, name_c_string.as_ptr() as *const _);

    let phys_c_string = ffi::CString::new("skvm").unwrap();
    glue::libevdev_set_phys(evdev, phys_c_string.as_ptr() as *const _);

    for capability in &device.capabilities {
        let ret = match *capability {
            Capability::ABS { code, info } => {
                let absinfo = glue::input_absinfo {
                    value: info.value,
                    minimum: info.minimum,
                    maximum: info.maximum,
                    fuzz: info.fuzz,
                    flat: info.flat,
                    resolution: info.resolution,
                };
                glue::libevdev_enable_event_code(
                    evdev,
                    glue::EV_ABS,
                    code as _,
                    &absinfo as *const glue::input_absinfo as *const _,
                )
            },
            Capability::REP { code, value } => {
                glue::libevdev_enable_event_code(
                    evdev,
                    glue::EV_REP,
                    code as _,
                    &value as *const i32 as *const _,
                )
            },
            Capability::Other { type_, code } => {
                glue::libevdev_enable_event_code(
                    evdev,
                    type_ as _,
                    code as _,
                    std::ptr::null_mut(),
                )
            },
        };
        if ret < 0 {
            println!("error enabling capability {:?}", capability);
            return Err(Error::from_raw_os_error(-ret));
        }
    }

    Ok(())
}
