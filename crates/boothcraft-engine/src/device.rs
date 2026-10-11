use crate::shared::{Renderer, Shared};
use boothcraft_core::Document;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("no output device available")]
    NoDevice,
    #[error("audio device error: {0}")]
    Device(String),
}

/// Owns the output stream. Keep it alive on the thread that created it (some backends are `!Send`).
pub struct Engine {
    shared: Arc<Shared>,
    _stream: Stream,
    pub device_name: String,
    pub sample_rate: u32,
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
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, &shared),
            SampleFormat::I16 => build::<i16>(&device, &config, &shared),
            SampleFormat::I32 => build::<i32>(&device, &config, &shared),
            SampleFormat::U16 => build::<u16>(&device, &config, &shared),
            other => Err(EngineError::Device(format!(
                "unsupported sample format {other}"
            ))),
        }?;
        stream
            .play()
            .map_err(|e| EngineError::Device(e.to_string()))?;
        Ok(Engine {
            shared,
            _stream: stream,
            device_name,
            sample_rate: config.sample_rate,
        })
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
) -> Result<Stream, EngineError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let mut renderer = Renderer::new(shared.clone());
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
