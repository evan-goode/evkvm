use crate::event::{Event, Device};
use crate::linux::event_reader::{EventReader, OpenError};
use crate::linux::event_writer::EventWriter;
use futures::StreamExt;
use inotify::{Inotify, WatchMask};
use std::io::{Error, ErrorKind};
use std::path::Path;
use std::time::Duration;
use std::collections::HashMap;
use tokio::fs;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot::{self, Receiver};
use tokio::time;

const EVENT_PATH: &str = "/dev/input";
const LOCAL_DEVICE_ID: u16 = u16::MAX;

pub struct EventManager {
    pub devices: HashMap<u16, Device>,
    pub writers: HashMap<u16, EventWriter>,
    event_receiver: UnboundedReceiver<Result<Event, Error>>,
    watcher_receiver: Receiver<Error>,
}

impl EventManager {
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
        let writers: HashMap<u16, EventWriter> = HashMap::new();

        // Sleep for a while to give userspace time to register our devices.
        // time::sleep(Duration::from_secs(1)).await;

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

        Ok(EventManager {
            devices,
            writers,
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

            // update manager state
        match event_result {
            Ok(Event::NewDevice(ref device)) => {
                println!("adding device {:?}", &device);
                self.devices.insert(device.id, device.clone());
            },
            Ok(Event::RemoveDevice(device_id)) => {
                self.devices.remove(&device_id);
            },
            _ => {},
        }


        event_result
    }

    pub async fn write(&mut self, event: Event) -> Result<(), Error> {
        match event {
            Event::Input { device_id, input } => {
                match self.writers.get_mut(&device_id) {
                    Some(writer) => writer.write(input).await,
                    _ => Ok(()),
                }
            },
            Event::NewDevice(device) => {
                let id = device.id;
                let writer = EventWriter::new(device).await?;
                self.writers.insert(id, writer);
                Ok(())
            },
            Event::RemoveDevice(device_id) => {
                self.writers.remove(&device_id);
                Ok(())
            },
        }
    }
}

async fn spawn_reader(
    path: &Path,
    sender: UnboundedSender<Result<Event, Error>>,
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

    let reader = match EventReader::open(&path).await {
        Ok(reader) => reader,
        Err(OpenError::Io(err)) => return Err(err),
        Err(OpenError::AlreadyOpened) => return Ok(()),
    };

    let event = Event::NewDevice(reader.device.clone());
    sender.send(Ok(event)).unwrap();

    tokio::spawn(handle_events(reader, sender));

    Ok(())
}

async fn handle_notify(sender: UnboundedSender<Result<Event, Error>>) -> Result<(), Error> {
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

async fn handle_events(mut reader: EventReader, sender: UnboundedSender<Result<Event, Error>>) {
    loop {
        let result = match reader.read().await {
            Ok(input_event) => {
                let event = Event::Input {
                    device_id: reader.device.id,
                    input: input_event,
                };
                sender.send(Ok(event)).is_ok()
            }
            // This happens if the device is disconnected.
            // In that case simply terminate the reading task.
            Err(ref err) if err.raw_os_error() == Some(libc::ENODEV) => {
                println!("fuck device removed");
                let event = Event::RemoveDevice(reader.device.id);
                let _ = sender.send(Ok(event));
                false
            },
            Err(err) => {
                println!("big fuck");
                let _ = sender.send(Err(err));
                false
            },
        };

        if !result {
            break;
        }
    }
}
