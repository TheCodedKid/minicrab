# minicrab

A desktop app for managing NetMD MiniDisc devices, written in Rust with
[Dioxus](https://dioxuslabs.com). Inspired by
[Web MiniDisc](https://github.com/asivery/webminidisc).

The NetMD protocol (USB transport, secure session, DES track encryption) comes from the
[`minidisc`](https://crates.io/crates/minidisc) crate, a Rust port of `netmd-js`.

## Features

- Detect and connect to NetMD units over USB
- Show the disc title, groups, tracks (mode, length, copy protection) and free space in SP/LP2/LP4
- Rename the disc and tracks, reorder tracks, delete tracks, erase the disc, eject
- Playback controls with a live position display; double-click a track to play it
- Send audio to the disc: WAV, FLAC, MP3, AAC/M4A, Ogg/Vorbis and more. Drag files onto the
  window or use **Add tracks**
  - **SP**: the app sends 44.1 kHz PCM and the device encodes it
  - **LP2 / LP4**: encoded to ATRAC3 on the computer with [`atracdenc`](https://github.com/dcherednik/atracdenc),
    which must be on `PATH`, next to the executable, or pointed to by `$ATRACDENC`
- Copy tracks back to the computer (`.aea` for SP, ATRAC3 `.wav` for LP). Only the Sony MZ-RH1 / M200 support this

## Running

```sh
cargo run --release
```

On Linux, give your user access to the device with a udev rule, for example
`SUBSYSTEM=="usb", ATTRS{idVendor}=="054c", MODE="0666"`. On Windows, the device needs the
WinUSB driver (install it with [Zadig](https://zadig.akeo.ie/)).

## Layout

| File | Purpose |
| --- | --- |
| `src/device.rs` | Device thread: owns the NetMD connection, runs commands, polls status |
| `src/audio.rs` | Decoding/resampling (symphonia + rubato), `atracdenc` integration, file headers |
| `src/model.rs` | Data types and the command/event protocol between UI and device thread |
| `src/ui.rs` | Dioxus components |
| `src/style.css` | Styling (light and dark) |

All device I/O runs on its own thread because `minidisc` blocks with `std::thread::sleep`
inside its async functions on native targets.

## Not yet implemented

- Creating, renaming or deleting groups. Groups are shown, but moving or deleting tracks on a
  grouped disc does not update the group ranges
- Hi-MD mode (non-NetMD) and factory-mode features
- A web (WebUSB) build. `minidisc` and `cross_usb` support wasm, but the device thread and
  file handling would need a wasm path
