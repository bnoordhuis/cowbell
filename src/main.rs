use std::path::PathBuf;
use std::process::ExitCode;

use cowbell::engine::Engine;
use cowbell::sample::Sample;
use cowbell::{demo, synth};

const USAGE: &str = "\
usage:
  cowbell [PROJECT.cowbell] [--break FILE.wav]
                                           open the tracker, and the project
                                           if it exists
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
    let mut args = Args {
        command: String::from("ui"),
        brk: None,
        seconds: 0.0,
        out: None,
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--break" => args.brk = Some(it.next().ok_or("--break needs a file")?.into()),
            "--seconds" => {
                let s = it.next().ok_or("--seconds needs a number")?;
                args.seconds = s.parse().map_err(|_| format!("bad --seconds: {s}"))?;
            }
            "-h" | "--help" => args.command = "help".into(),
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            "render" | "play" | "help" if args.command == "ui" && args.out.is_none() => {
                args.command = arg
            }
            _ => args.out = Some(arg.into()),
        }
    }
    Ok(args)
}

fn load_break(path: Option<&PathBuf>) -> Result<Sample, String> {
    match path {
        // Voices resample, so the break's rate needn't match the device's.
        None => Ok(synth::demo_break(48_000, demo::BPM as f32)),
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
    let song = demo::song(load_break(args.brk.as_ref())?);
    let seconds = if args.seconds > 0.0 {
        args.seconds
    } else {
        // Once through the order list.
        let beats: f64 = song
            .order
            .iter()
            .map(|&p| {
                let p = &song.patterns[p];
                p.rows as f64 / p.rows_per_beat as f64
            })
            .sum();
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
    use cowbell::engine::Command;

    let song = demo::song(load_break(args.brk.as_ref())?);
    let (output, mut controller) = cowbell::audio::start(song)?;
    controller
        .send(Command::Play)
        .map_err(|_| "command queue full")?;
    eprintln!(
        "playing on {} at {} Hz, press enter to stop",
        output.device, output.sample_rate
    );
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(feature = "live"))]
fn play(_: &Args) -> Result<(), String> {
    Err("built without live audio; rebuild with --features live".into())
}

#[cfg(feature = "ui")]
fn ui(args: &Args) -> Result<(), String> {
    // An existing project opens; a new path is where the demo song will be
    // saved.
    let song = match &args.out {
        Some(path) if path.exists() => cowbell::project::load(path)?,
        _ => demo::song(load_break(args.brk.as_ref())?),
    };
    cowbell::ui::run(song, args.out.clone())
}

#[cfg(not(feature = "ui"))]
fn ui(_: &Args) -> Result<(), String> {
    Err("built without the UI; rebuild with --features ui".into())
}

fn main() -> ExitCode {
    let result = parse_args().and_then(|args| match args.command.as_str() {
        "render" => render(&args),
        "play" => play(&args),
        "ui" => ui(&args),
        "help" => {
            println!("{USAGE}");
            Ok(())
        }
        _ => unreachable!("parse_args only sets known commands"),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cowbell: {e}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
