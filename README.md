# Boothcraft

An open-source, cross-platform audio editor in Rust, aiming for feature parity with the waveform
editor of professional desktop audio tools. Multitrack sessions are out of scope.

## Status
<img width="1392" height="904" alt="Screenshot 2026-10-11 at 00 14 50" src="https://github.com/user-attachments/assets/4ad31479-df69-4c21-8cc5-f7576343400b" />Early. Milestone 0 and part of milestone 1 are in place; see [docs/parity.md](docs/parity.md).

## Build and run

```sh
cargo run -p boothcraft-app -- path/to/file.wav   # GUI
cargo run -p boothcraft-cli -- info file.flac     # headless tools
cargo test --workspace
```

## Layout

| Crate | Role |
|---|---|
| `boothcraft-core` | Immutable block storage, edits, selection, markers, undo history |
| `boothcraft-io` | Decode (symphonia) and encode WAV / AIFF / FLAC |
| `boothcraft-dsp` | Effects and analysis (not started) |
| `boothcraft-engine` | Real-time output, transport, metering |
| `boothcraft-ui` | egui panels, waveform view, edit commands |
| `boothcraft-app` | GUI binary |
| `boothcraft-cli` | Headless `info` / `convert` |
