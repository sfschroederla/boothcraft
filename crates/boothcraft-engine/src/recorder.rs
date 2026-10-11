//! Audio input: a cpal input stream feeding a lock-free ring buffer that the UI thread drains.

use crate::device::EngineError;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

/// Seconds of audio the ring buffer can hold before the UI must drain it.
const RING_SECONDS: usize = 4;

pub struct Recorder {
    _stream: Stream,
    consumer: rtrb::Consumer<f32>,
    dropped: Arc<AtomicUsize>,
    pub sample_rate: u32,
    pub channels: usize,
    pub device_name: String,
}

impl Recorder {
    /// Open the default input device and start capturing.
    pub fn start() -> Result<Recorder, EngineError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(EngineError::NoInputDevice)?;
        let supported = device
            .default_input_config()
            .map_err(|e| EngineError::Device(e.to_string()))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels as usize;
        let (producer, consumer) =
            rtrb::RingBuffer::new(config.sample_rate as usize * channels * RING_SECONDS);
        let dropped = Arc::new(AtomicUsize::new(0));
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, producer, dropped.clone()),
            SampleFormat::I16 => build::<i16>(&device, &config, producer, dropped.clone()),
            SampleFormat::I32 => build::<i32>(&device, &config, producer, dropped.clone()),
            SampleFormat::U16 => build::<u16>(&device, &config, producer, dropped.clone()),
            other => Err(EngineError::Device(format!(
                "unsupported input format {other}"
            ))),
        }?;
        stream
            .play()
            .map_err(|e| EngineError::Device(e.to_string()))?;
        let device_name = device
            .id()
            .map(|i| i.to_string())
            .unwrap_or_else(|_| "default".into());
        Ok(Recorder {
            _stream: stream,
            consumer,
            dropped,
            sample_rate: config.sample_rate,
            channels,
            device_name,
        })
    }

    /// Move everything captured so far into `out` (interleaved).
    pub fn drain(&mut self, out: &mut Vec<f32>) {
        let n = self.consumer.slots();
        if let Ok(chunk) = self.consumer.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            out.extend_from_slice(a);
            out.extend_from_slice(b);
            chunk.commit_all();
        }
    }

    /// Samples lost because the ring buffer was full (the UI fell behind).
    pub fn dropped(&self) -> usize {
        self.dropped.load(Relaxed)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut producer: rtrb::Producer<f32>,
    dropped: Arc<AtomicUsize>,
) -> Result<Stream, EngineError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device
        .build_input_stream(
            *config,
            move |data: &[T], _| {
                for &s in data {
                    if producer.push(f32::from_sample(s)).is_err() {
                        dropped.fetch_add(1, Relaxed);
                    }
                }
            },
            |e| eprintln!("audio input error: {e}"),
            None,
        )
        .map_err(|e| EngineError::Device(e.to_string()))
}
