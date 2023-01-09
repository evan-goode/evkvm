# evkvm

### **evkvm is alpha software! It might not be ready for use yet.**

evkvm is a tool for sharing input devices among multiple Linux machines.
It follows a client/server architecture, where the server, or "sender", relays events (mouse movement, key press, ...) to clients, or "receivers".
evkvm is a fork of [rkvm](https://github.com/htrefil/rkvm) and adds some features while sacrificing support for Windows clients.

Switching between different clients is done by a configurable keyboard shortcut.

## Features
- Mandatory TLS encryption, backed by [Rustls](https://github.com/rustls/rustls)
- Supports all input devices (touchpads, gamepads, drawing tablets, etc.), not just keyboards and mice.
- Display server agnostic
- Low overhead

## Requirements
- Rust 1.48 or higher

## Build requirements
- The uinput Linux kernel module, available by default in most distros
- pkgconfig
- libevdev
- clang (for libclang)

## Building

1. Install build dependencies

	On Debian-based systems:

	```
	sudo apt install build-essential libclang-dev pkg-config rustc libevdev-dev
	```

	On Arch Linux:

	```
	sudo pacman -S clang pkg-config rust libevdev
	```

	Or, using Nix, simply run

	```
	nix-shell
	```

2. Then, build using `cargo`:

	```
	cargo build --release
	```

## Manual installation

<!-- Packages are currently available for Arch Linux and NixOS. If you use another distribution, you can install `evkvm` manually. -->

1. Follow the steps above to build `evkvm`. Then, install the binary:

	```
	sudo cp ./target/release/evkvm /usr/bin/evkvm
	```

2. Install the example config file

	```
	sudo install -D ./example/config.toml /etc/evkvm/config.toml
	```

3. Add an `evkvm` user and add them to the `input` group

	```
	sudo useradd --system --user-group --groups input evkvm
	```

4. Make sure the `uinput` kernel module is loaded

	```
	sudo cp ./example/evkvm-uinput.conf /etc/modules-load.d/evkvm-uinput.conf
	sudo modprobe uinput
	```

5. Allow members of the `input` group to access `/dev/uinput`

	```
	sudo cp ./example/40-evkvm-uinput.rules /etc/udev/rules.d/40-evkvm-uinput.rules
	sudo udevadm control --reload-rules
	```

5. Install the systemd unit and enable it

	```
	sudo cp ./example/evkvm.service /etc/systemd/system/evkvm.service
	sudo systemctl enable --now evkvm
	```

## Setup

After installing and starting `evkvm` on two systems, it's time to link them together.
You will need to know the "fingerprint" of each device so they can authenticate each other.

1. ### On both the sender and receiver:

	Run `sudo evkvm fingerprint`.

2. ### On the sender:

	Edit `/etc/evkvm/config.toml` and append the following, replacing the template values with your own:

	```
	[[receivers]]
	nick = "A NICKNAME FOR THE RECEIVER"
	fingerprint = "OUTPUT OF `evkvm fingerprint` ON THE RECEIVER"
	```

3. ### On the receiver

	Edit `/etc/evkvm/config.toml` and append the following:

	```
	[[senders]]
	nick = "A NICKNAME FOR THE SENDER"
	address = "IP ADDRESS OR DOMAIN NAME OF THE SENDER"
	fingerprint = "OUTPUT OF `evkvm fingerprint` ON THE SENDER"
	```

4. ### On both the sender and receiver:

	Restart `evkvm` for the changes to take effect:

	```
	sudo systemctl restart evkvm
	```

Pressing the switch shortcut (Left Alt + Right Alt by default) on the sender should now start forwarding inputs to the receiver. Pressing it again should switch back.
To troubleshoot, you can watch the logs on each system using `sudo journalctl -fu evkvm`.

## Configuration

By default, evkvm reads its config file from `/etc/evkvm/config.toml`. A different config file can be passed with the `--config-path` option.

### Options in config.toml

- `listen-address`: for senders, the address and port to bind to. Default is `"0.0.0.0:5258"`.
- `switch-keys`: for senders, the keyboard shortcut that triggers switching among receivers. Default is `["LeftAlt", "RightAlt"]`. See `keys.md` for a list of key names.
- `identity-path`: the path to the device's identity file. Default is `/var/lib/evkvm/identity.pem`.
- `senders`: for receivers, an array of devices that can forward inputs to this device
	+ `nick`: a nickname for the device
	+ `address`: the IP address or domain name to connect to
	+ `port`: the port to connect to. Default is `5258`.
	+ `fingerprint`: the TLS fingerprint of the sender, used for authentication. Run `sudo evkvm fingerprint` on the sender to get this value.
- `receivers`: for senders, an array of devices that can receive inputs from this device
	+ `nick`: a nickname for the device
	+ `fingerprint`: the TLS fingerprint of the receiver, used for authentication. Run `sudo evkvm fingerprint` on the receiver to get this value.

Note that any device running evkvm can function as both a sender and receiver, depending on the senders and receivers configured in `config.toml`.
Receivers can connect to any number of senders, and senders can send events to any number of receivers.

## Comparison with rkvm

Compared with [rkvm](https://github.com/htrefil/rkvm), evkvm has the following advantages:

- Supports all input devices (touchpads, gamepads, drawing tablets, etc.), not just keyboards and mice.
- Receivers can connect to multiple senders
- Receivers auto-reconnect to senders
- Generating certificates/keys for authentication is done automatically when evkvm is first run.
Using `rkvm`, you'd need to do this step manually.
evkvm authenticates devices using certificate fingerprints, which are easy to copy and paste around.

The biggest disadvantage of evkvm is that it doesn't support Windows.
Unfortunately, touchpads and other input devices would have been too difficult to support following rkvm's cross-platform approach.
evkvm sends raw evdev events rather than using a higher-level, platform-neutral encoding like rkvm does.
Support for FreeBSD (and maybe other BSD?) systems should be possible, however, since FreeBSD uses evdev.

## Comparison with Input Leap/Barrier/Synergy

evkvm and Input Leap operate on completely different levels of the input stack and thus are very different programs.

Some advantages of evkvm compared with Input Leap:

- evkvm supports all input devices, including gamepads.
- evkvm has first-class support for touchpads.
High-resolution scrolling and gestures such as pinch-to-zoom work great.
- evkvm works on Wayland, Xorg, TTYs and more.
It doesn't know or care about display servers---it's much closer to a physical KVM switch, unplugging input devices from the sender and plugging them into the receiver.
- Supports all keyboard layouts.
- Probably lower latency? Not tested yet.

Some disadvantages:

- evkvm doesn't do clipboard sharing (yet?)
- evkvm has to be run by a highly privileged user who can access `/dev/uinput` and nodes under `/dev/input/`.
This is fine for single-user systems, but it's not great for enterprise environments or shared computers.
- evkvm can't switch receivers when the cursor moves off the screen.
The keyboard shortcut is currently the only way to switch.
- evkvm is only available on Linux.
- Input Leap is much more mature.

## Project structure
- `evkvm` - main application code
- `input` - handles reading from and writing to input devices
- `net` - network protocol encoding and decoding

[Bincode](https://github.com/servo/bincode) is used for encoding of messages on the network and [Tokio](https://tokio.rs) as an asynchronous runtime.

## Contributions
All contributions, including both PRs and issues, are very welcome.

## License
[MIT](LICENSE)

The original license for rkvm is included as [LICENSE.rkvm](https://github.com/evan-goode/evkvm/blob/master/LICENSE.rkvm).
