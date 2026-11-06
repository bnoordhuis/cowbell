use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cowbell::engine::Engine;
use cowbell::sample::Sample;
use cowbell::{demo, synth};

const USAGE: &str = "\
usage:
  cowbell [PROJECT.cowbell] [--break FILE.wav]
                                           open the window, and the project
                                           if it exists
  cowbell render [PROJECT.cowbell] [--seconds N] OUT.wav
                                           export the project (or the demo)
                                           once through, with its tail;
                                           --seconds renders a fixed length
  cowbell play   [--break FILE.wav]        (needs --features live)

Plays the demo song. --break slices your own break instead of the built-in one.";

struct Args {
    command: String,
    brk: Option<PathBuf>,
    seconds: f64,
    /// File arguments, in order.
    files: Vec<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut args = Args {
        command: String::from("ui"),
        brk: None,
        seconds: 0.0,
        files: Vec::new(),
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
            "render" | "play" | "help" if args.command == "ui" && args.files.is_empty() => {
                args.command = arg
            }
            _ => args.files.push(arg.into()),
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

fn is_project(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e == cowbell::project::EXTENSION)
}

fn render(args: &Args) -> Result<(), String> {
    let (song, out) = match args.files.as_slice() {
        [project, out] if is_project(project) => (cowbell::project::load(project)?, out),
        [out] if !is_project(out) => (demo::song(load_break(args.brk.as_ref())?), out),
        _ => return Err("render needs [PROJECT.cowbell] OUT.wav".into()),
    };
    if args.seconds > 0.0 {
        // A fixed length, looping the song if it's shorter.
        const RATE: u32 = cowbell::export::RATE;
        let (mut engine, _controller) = Engine::new(RATE, song);
        engine.play();
        let mut buf = vec![0.0f32; 2 * (args.seconds * RATE as f64).round() as usize];
        engine.process(&mut buf);
        let frames = buf.as_chunks::<2>().0.to_vec();
        Sample::new("render", RATE, frames)
            .save_wav(out)
            .map_err(|e| format!("{}: {e}", out.display()))?;
        eprintln!("wrote {:.2}s to {}", args.seconds, out.display());
    } else {
        let seconds = cowbell::export::export_wav(&song, out)?;
        eprintln!("wrote {seconds:.2}s to {}", out.display());
    }
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
    let project = args.files.first();
    let song = match project {
        Some(path) if path.exists() => cowbell::project::load(path)?,
        _ => demo::song(load_break(args.brk.as_ref())?),
    };
    cowbell::ui::run(song, project.cloned())
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
