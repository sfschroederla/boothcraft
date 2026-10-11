# Parity checklist

Status: `done`, `partial`, `todo`. Milestones refer to the project plan (M0–M7).

## M0 / M1: editor core
| Feature | Status | Notes |
|---|---|---|
| Open WAV / FLAC / MP3 / OGG / AAC | partial | decoding via symphonia; only WAV/AIFF/FLAC round-trip tested |
| Save WAV / AIFF / FLAC | done | WAV float32 default; 16/24-bit available via the library and CLI |
| Dockable panels | done | Editor, Files, History, Levels, Selection/View |
| Waveform view, zoom, scroll, overview bar | partial | drawn with egui's painter, not yet a custom GPU pass |
| Amplitude scale (dB / percent / normalized / 16-bit) with gridlines | done | drag the scale to zoom vertically, double-click resets |
| Amplitude zoom buttons | done | 0.25x to 256x |
| Time ruler with major/minor ticks and vertical gridlines | done | minutes:seconds, decimal, samples, SMPTE 30 fps |
| Zoom controls (in, out, to selection, full) | done | toolbar above the waveform |
| Adjustable selection edges, shift-extend | done | grab within 6 px of an edge; shift+click or shift+drag extends |
| Channel labels in lanes | done | |
| Time selection, cursor, select all | done | |
| Cut / copy / paste / mix paste / delete / crop | done | 5 clipboards |
| Silence / invert / reverse / amplify / insert silence | done | |
| Undo / redo and History panel | done | whole-document snapshots with shared blocks |
| Channel enable (edit one channel) | partial | delete on a subset of channels silences instead of shortening |
| Playback, loop, seek, resampling to device rate | partial | linear-interpolation resampler |
| Level meters with peak hold | done | output peaks |
| Markers panel | todo | model exists in `boothcraft-core` |
| Recording and punch-in | todo | |
| Fade handles, HUD gain | todo | |
| Bars/beats ruler format | todo | needs tempo settings |
| Snap to zero-crossings / markers | todo | |
| Dither and sample-rate conversion on export | todo | |
| Disk-backed block store | todo | all audio is held in RAM today |

## M2–M7
Effects, spectral display, noise reduction, analysis, batch, plugin hosting and packaging are all `todo`.
