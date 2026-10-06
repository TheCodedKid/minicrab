# minicrab

A desktop NetMD MiniDisc manager built with Rust and [Dioxus](https://dioxuslabs.com).

## Features

- View disc, group, track, and capacity information
- Rename, reorder, delete, erase, and eject
- Control playback
- Import WAV, FLAC, MP3, AAC/M4A, Ogg/Vorbis, and other formats
- Record in SP, LP2, or LP4
- Export tracks with a Sony MZ-RH1/M200

LP2/LP4 recording requires [atracdenc](https://github.com/dcherednik/atracdenc) on `PATH`, beside the app, or set through `ATRACDENC`.

## Run

```sh
cargo run --release
```

Linux requires USB permissions, for example:

```text
SUBSYSTEM=="usb", ATTRS{idVendor}=="054c", MODE="0666"
```

Windows requires the WinUSB driver, available through [Zadig](https://zadig.akeo.ie/).

## Limitations

Group editing, Hi-MD mode, factory-mode features, and WebUSB are not supported.
