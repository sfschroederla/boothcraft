# Boothcraft

An open-source, cross-platform audio editor in Rust, aiming for feature parity with the waveform
editor of professional desktop audio tools. Multitrack sessions are out of scope.

## Status
<img width="1392" height="904" alt="Screenshot 2026-10-11 at 00 14 50" src="https://github.com/user-attachments/assets/4ad31479-df69-4c21-8cc5-f7576343400b" />Milestones 1 (the core editor, apart from a disk-backed block store) and 2 (the effects framework and first 22 effects) are in place. See [docs/parity.md](docs/parity.md).

## Build and run

```sh
cargo run -p boothcraft-app -- path/to/file.wav   # GUI
cargo run -p boothcraft-cli -- info file.flac     # headless tools
cargo run -p boothcraft-cli -- effects            # list effects and parameters
cargo run -p boothcraft-cli -- apply --effect hard_limiter --param ceiling=-1 in.wav out.wav
cargo test --workspace
```

## Layout

| Crate | Role |
|---|---|
| `boothcraft-core` | Immutable block storage, edits, selection, markers, undo history |
| `boothcraft-io` | Decode (symphonia) and encode WAV / AIFF / FLAC |
| `boothcraft-dsp` | Effects, Effects Rack, presets, generators, loudness, sample-rate conversion, dither |
| `boothcraft-engine` | Real-time output, transport, metering |
| `boothcraft-ui` | egui panels, waveform view, edit commands |
| `boothcraft-app` | GUI binary |
| `boothcraft-cli` | Headless `info` / `convert` |

## macOS app bundle

```sh
./scripts/macos_bundle.sh        # builds only boothcraft-app, not boothcraft-cli
open target/release/bundle/osx/Boothcraft.app
```

## Recording on macOS

Recording uses the default input device. Run from a terminal, the app inherits that terminal's
microphone permission (System Settings > Privacy & Security > Microphone); without it, macOS
delivers silence. The bundled `.app` above declares its own `NSMicrophoneUsageDescription`, so macOS
prompts for microphone access the first time it records, independent of the terminal's permission.
