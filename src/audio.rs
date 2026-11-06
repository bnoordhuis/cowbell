//! Live audio output: runs an [`Engine`] in the cpal callback.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::engine::{Controller, Engine};
use crate::song::Song;

/// Largest block the engine renders at once; the callback splits bigger ones.
const MAX_BLOCK: usize = 4096;

/// A running output stream. Audio stops when this is dropped.
pub struct Output {
    _stream: cpal::Stream,
    pub sample_rate: u32,
    pub device: String,
}

/// Open the default output device and start the engine on it.
pub fn start(song: Song) -> Result<(Output, Controller), String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no output device")?;
    let name = device
        .id()
        .map_or_else(|_| "unknown device".into(), |id| id.to_string());
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let sample_rate = config.sample_rate;
    let (engine, controller) = Engine::new(sample_rate, song);

    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, engine)?,
        SampleFormat::I16 => build::<i16>(&device, config, engine)?,
        SampleFormat::I32 => build::<i32>(&device, config, engine)?,
        SampleFormat::U16 => build::<u16>(&device, config, engine)?,
        other => return Err(format!("unsupported sample format {other}")),
    };
    stream.play().map_err(|e| e.to_string())?;
    Ok((
        Output {
            _stream: stream,
            sample_rate,
            device: name,
        },
        controller,
    ))
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: StreamConfig,
    mut engine: Engine,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    // Allocated here, not in the callback.
    let mut scratch = vec![0.0f32; 2 * MAX_BLOCK];
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
                for chunk in out.chunks_mut(channels * MAX_BLOCK) {
                    let frames = chunk.len() / channels;
                    let stereo = &mut scratch[..2 * frames];
                    engine.process(stereo);
                    for (dst, src) in chunk
                        .chunks_exact_mut(channels)
                        .zip(stereo.as_chunks::<2>().0)
                    {
                        if channels == 1 {
                            dst[0] = T::from_sample(0.5 * (src[0] + src[1]));
                            continue;
                        }
                        dst[0] = T::from_sample(src[0]);
                        dst[1] = T::from_sample(src[1]);
                        for d in &mut dst[2..] {
                            *d = T::from_sample(0.0);
                        }
                    }
                }
            },
            |err| eprintln!("stream error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())
}
