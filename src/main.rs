use std::path::PathBuf;
use std::process::ExitCode;

use cowbell::engine::Engine;
use cowbell::sample::Sample;
use cowbell::{demo, synth};

const USAGE: &str = "\
usage:
  cowbell render [--break FILE.wav] [--seconds N] OUT.wav
  cowbell play   [--break FILE.wav]        (needs --features live)

Plays the demo song. --break slices your own break instead of the built-in one.";

struct Args {
    command: String,
    brk: Option<PathBuf>,
    seconds: f64,
    out: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().ok_or("missing command")?;
    let mut args = Args { command, brk: None, seconds: 0.0, out: None };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--break" => args.brk = Some(it.next().ok_or("--break needs a file")?.into()),
            "--seconds" => {
                let s = it.next().ok_or("--seconds needs a number")?;
                args.seconds = s.parse().map_err(|_| format!("bad --seconds: {s}"))?;
            }
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            _ => args.out = Some(arg.into()),
        }
    }
    Ok(args)
}

fn load_break(path: Option<&PathBuf>, sample_rate: u32) -> Result<Sample, String> {
    match path {
        None => Ok(synth::demo_break(sample_rate, demo::BPM as f32)),
        Some(path) => {
            let mut s = Sample::load_wav(path).map_err(|e| format!("{}: {e}", path.display()))?;
            s.detect_slices(0.6, 60.0);
            eprintln!("{}: {} slices", s.name, s.slices.len());
            Ok(s)
        }
    }
}

fn render(args: &Args) -> Result<(), String> {
    const RATE: u32 = 48_000;
    let out = args.out.as_ref().ok_or("render needs an output file")?;
    let song = demo::song(load_break(args.brk.as_ref(), RATE)?);
    let seconds = if args.seconds > 0.0 {
        args.seconds
    } else {
        // Once through the order list.
        let beats: f64 = song.order.iter().map(|&p| {
            let p = &song.patterns[p];
            p.rows as f64 / p.rows_per_beat as f64
        }).sum();
        beats * 60.0 / song.bpm
    };
    let (mut engine, _controller) = Engine::new(RATE, song);
    engine.play();

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let err = |e: hound::Error| format!("{}: {e}", out.display());
    let mut writer = hound::WavWriter::create(out, spec).map_err(err)?;
    let mut left = (seconds * RATE as f64).round() as usize;
    let mut buf = vec![0.0f32; 2 * 512];
    while left > 0 {
        let n = left.min(512);
        engine.process(&mut buf[..2 * n]);
        for &s in &buf[..2 * n] {
            writer.write_sample(s).map_err(err)?;
        }
        left -= n;
    }
    writer.finalize().map_err(err)?;
    eprintln!("wrote {:.2}s to {}", seconds, out.display());
    Ok(())
}

#[cfg(feature = "live")]
fn play(args: &Args) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
    use cowbell::engine::Command;

    fn run<T: SizedSample + FromSample<f32>>(
        device: &cpal::Device,
        config: StreamConfig,
        mut engine: Engine,
    ) -> Result<cpal::Stream, String> {
        let channels = config.channels as usize;
        // Allocated here, not in the callback.
        let mut scratch = vec![0.0f32; 2 * 4096];
        let stream = device
            .build_output_stream(
                config,
                move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
                    for chunk in out.chunks_mut(channels * 4096) {
                        let frames = chunk.len() / channels;
                        let stereo = &mut scratch[..2 * frames];
                        engine.process(stereo);
                        for (dst, src) in chunk.chunks_exact_mut(channels).zip(stereo.chunks_exact(2)) {
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
            .map_err(|e| e.to_string())?;
        Ok(stream)
    }

    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no output device")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let song = demo::song(load_break(args.brk.as_ref(), config.sample_rate)?);
    let (engine, mut controller) = Engine::new(config.sample_rate, song);

    let stream = match format {
        SampleFormat::F32 => run::<f32>(&device, config, engine)?,
        SampleFormat::I16 => run::<i16>(&device, config, engine)?,
        SampleFormat::I32 => run::<i32>(&device, config, engine)?,
        SampleFormat::U16 => run::<u16>(&device, config, engine)?,
        other => return Err(format!("unsupported sample format {other}")),
    };
    stream.play().map_err(|e| e.to_string())?;
    controller.send(Command::Play).map_err(|_| "command queue full")?;

    eprintln!("playing, press enter to stop");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    let _ = controller.send(Command::Stop);
    controller.collect_garbage();
    Ok(())
}

#[cfg(not(feature = "live"))]
fn play(_: &Args) -> Result<(), String> {
    Err("built without live audio; rebuild with --features live".into())
}

fn main() -> ExitCode {
    let result = parse_args().and_then(|args| match args.command.as_str() {
        "render" => render(&args),
        "play" => play(&args),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other}")),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cowbell: {e}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
