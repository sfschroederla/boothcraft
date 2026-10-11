use crate::prefetch::Prefetcher;
use crate::rackctl::{rack_channel, RackControl, RackLink};
use crate::shared::{Renderer, Shared};
use boothcraft_core::Document;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("no output device available")]
    NoDevice,
    #[error("no input device available")]
    NoInputDevice,
    #[error("audio device error: {0}")]
    Device(String),
}

/// Owns the output stream. Keep it alive on the thread that created it (some backends are `!Send`).
pub struct Engine {
    shared: Arc<Shared>,
    _stream: Stream,
    /// Kept alive for the engine's lifetime; stops its thread on drop.
    _prefetcher: Prefetcher,
    rack: Option<RackControl>,
    tap: Option<Tap>,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: usize,
}

/// UI-side end of the output tap.
pub struct Tap {
    pub consumer: rtrb::Consumer<f32>,
    pub channels: usize,
    pub sample_rate: u32,
}

impl Tap {
    /// Move everything available into `out`.
    pub fn drain(&mut self, out: &mut Vec<f32>) {
        let n = self.consumer.slots();
        if let Ok(chunk) = self.consumer.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            out.extend_from_slice(a);
            out.extend_from_slice(b);
            chunk.commit_all();
        }
    }
}

/// Frames rendered per call, bounding the callback's scratch buffer.
const MAX_FRAMES: usize = 4096;

impl Engine {
    pub fn new() -> Result<Engine, EngineError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(EngineError::NoDevice)?;
        let supported = device
            .default_output_config()
            .map_err(|e| EngineError::Device(e.to_string()))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let device_name = device
            .id()
            .map(|i| i.to_string())
            .unwrap_or_else(|_| "default".into());
        let shared = Shared::new();
        let (rack, link) = rack_channel(config.sample_rate, config.channels as usize);
        let (tap_tx, tap_rx) =
            rtrb::RingBuffer::new(config.sample_rate as usize * config.channels as usize * 2);
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, &shared, link, tap_tx),
            SampleFormat::I16 => build::<i16>(&device, &config, &shared, link, tap_tx),
            SampleFormat::I32 => build::<i32>(&device, &config, &shared, link, tap_tx),
            SampleFormat::U16 => build::<u16>(&device, &config, &shared, link, tap_tx),
            other => Err(EngineError::Device(format!(
                "unsupported sample format {other}"
            ))),
        }?;
        stream
            .play()
            .map_err(|e| EngineError::Device(e.to_string()))?;
        let prefetcher = Prefetcher::spawn(shared.clone());
        Ok(Engine {
            shared,
            _stream: stream,
            _prefetcher: prefetcher,
            rack: Some(rack),
            tap: Some(Tap {
                consumer: tap_rx,
                channels: config.channels as usize,
                sample_rate: config.sample_rate,
            }),
            device_name,
            sample_rate: config.sample_rate,
            channels: config.channels as usize,
        })
    }

    /// The output tap (everything played, interleaved); can be taken once.
    pub fn take_tap(&mut self) -> Option<Tap> {
        self.tap.take()
    }

    /// The preview-rack control; can be taken once.
    pub fn take_rack_control(&mut self) -> Option<RackControl> {
        self.rack.take()
    }

    /// Handle for the UI to control playback; cheap to clone.
    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    pub fn set_document(&self, doc: Arc<Document>) {
        self.shared.set_document(doc);
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: &Arc<Shared>,
    link: RackLink,
    tap: rtrb::Producer<f32>,
) -> Result<Stream, EngineError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let mut renderer = Renderer::new(shared.clone());
    renderer.attach_rack(link);
    renderer.attach_tap(tap);
    let mut buf = vec![0.0f32; MAX_FRAMES * channels];
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _| {
                for chunk in data.chunks_mut(MAX_FRAMES * channels) {
                    let tmp = &mut buf[..chunk.len()];
                    renderer.render(tmp, channels, rate);
                    for (o, s) in chunk.iter_mut().zip(tmp.iter()) {
                        *o = T::from_sample(*s);
                    }
                }
            },
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(|e| EngineError::Device(e.to_string()))
}

#[allow(dead_code)]
fn _assert_sample<T: Sample>() {}
