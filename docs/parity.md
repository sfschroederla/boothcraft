# Parity checklist

Status: `done`, `partial`, `todo`. Milestones refer to the project plan (M0–M7). "Unverified" means
built and unit-tested but not yet exercised by a person in the running app.

## M0 / M1: editor core
| Feature | Status | Notes |
|---|---|---|
| Open WAV / FLAC / MP3 / OGG / AAC | partial | decoding via symphonia; only WAV/AIFF/FLAC round-trip tested |
| Save WAV / AIFF / FLAC | done | Save As offers sample type, sample rate and dither; plain Save reuses them |
| File > New (rate, channels) | done | |
| Dockable panels, Window menu | done | Editor, Files, History, Selection/View, Markers, Phase Meter, Levels |
| Waveform view, zoom, scroll, overview bar | partial | drawn with egui's painter, not yet a custom GPU pass |
| Amplitude scale (dB / percent / normalized / 16-bit) with gridlines | done | drag the scale to zoom vertically, double-click resets, right-click for units |
| Time ruler: minutes:seconds, decimal, samples, SMPTE 30 fps, bars/beats | done | right-click the ruler; tempo and beats per bar for bars/beats |
| Zoom controls (time, amplitude, selection, full) | done | icons at the right of the transport row |
| Selection: drag, adjustable edges, shift-extend, select all | done | |
| Selection/View panel, editable | done | type a position in the current time format; red text = not parseable |
| Snapping (markers, ruler ticks, zero crossings), S toggles | done | Edit > Snapping |
| Edit > Zero Crossings (6 adjust commands) | done | searches up to 1 s away; uses the summed enabled channels |
| Cut / copy / paste / mix paste / delete / crop | done | 5 clipboards |
| Silence / invert / reverse / amplify / insert silence | done | |
| Undo / redo and History panel | done | whole-document snapshots with shared blocks |
| Channel enable (L/R buttons) | partial | delete on a subset of channels silences instead of shortening |
| Markers: cue and range, M to add, drag, rename, delete, prev/next | done | saved in WAV `cue `/`LIST adtl`; not yet in AIFF/FLAC |
| Fade in/out handles (log/linear/exp, Cmd for cosine) | done, unverified | applied on release |
| HUD gain knob with live preview | done, unverified | applied on release |
| Playback, loop, seek | partial | linear-interpolation resampling to the device rate |
| Play selection with pre-roll / post-roll | done | Edit > Preferences |
| Skip Selection | done | plays around the selection (at least 2 s each side) |
| Rewind / fast-forward | done | hold to scrub at 4x |
| Recording, punch-in over a selection | done, unverified | default input device only; live device capture not yet tested by a person |
| Level meters with peak hold, phase meter | done | input level shown while recording |
| Edit > Convert Sample Type (rate, mono/stereo) | done | |
| Disk-backed block store | todo | all audio is held in RAM today |

## M2: effects
| Feature | Status | Notes |
|---|---|---|
| Effects Rack: 16 slots, bypass, reorder, input/output gain, mix | done, unverified | Window > Effects Rack |
| Real-time preview through the rack or an effect dialog | done, unverified | lock-free handoff to the audio thread |
| Apply to selection (or whole file) on enabled channels | done | background job with progress and cancel; one undo step |
| Effect presets (save/load/delete) and rack presets (JSON) | done | stored in the user config directory |
| Amplify, Channel Mixer | done | |
| Normalize, Fade Envelope, Gain Envelope | done | offline; envelope point editor with shape presets |
| Single-band Compressor, Hard Limiter (look-ahead), DeEsser | done | limiter latency is compensated on Apply |
| Parametric Equalizer (response graph), Graphic EQ 10/20/30 band | done, unverified | |
| Notch Filter, Scientific Filter (Butterworth, Chebyshev I) | done | order 2–8; LP/HP/BP/BS |
| Delay, Echo, Analog Delay | done | |
| Chorus, Flanger, Phaser | done | |
| Reverb (Freeverb-style), Studio Reverb (FDN) | done | Audition's Reverb is convolution-based; ours is algorithmic |
| Generate: Tones (with sweep), Noise (white/pink/brown), DTMF, Silence | done | |
| Convert Sample Type | done | see M1 |
| Loudness panel (momentary, short-term, integrated, true peak) | done, unverified | BS.1770 via `ebur128` |
| Match Loudness (targets, peak limiting, several files) | done | one undo step per file |
| Multiband Compressor, Dynamics Processing, Tube-modeled Compressor, Speech Volume Leveler | todo | M5 |
| Convolution Reverb, Full Reverb, Surround Reverb | todo | M5 |
| FFT Filter, Graphic Phase Shifter, Stereo Expander, time/pitch | todo | M5 |

## M3–M7
Spectral display, noise reduction, analysis, batch, plugin hosting and packaging are all `todo`.
