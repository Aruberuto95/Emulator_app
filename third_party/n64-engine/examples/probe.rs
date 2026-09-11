//! Local-ROM probe: cargo run --release -p n64-engine --example probe -- ROM FRAMES OUTPUT_DIR
use std::{
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
    time::Instant,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(4..=6).contains(&args.len()) {
        return Err("expected ROM FRAMES OUTPUT_DIR [STATE_FILE] [INPUT_CSV]".into());
    }
    let rom = fs::read(&args[1])?;
    let frames: usize = args[2].parse()?;
    let dir = PathBuf::from(&args[3]);
    fs::create_dir_all(&dir)?;
    let mut engine = n64_engine::Engine::new(rom, 48000)?;
    if let Some(state) = args.get(4).filter(|p| !p.is_empty()) {
        engine.load_state(&fs::read(state)?)?;
    }
    let mut events = std::collections::BTreeMap::new();
    if let Some(path) = args.get(5) {
        for line in fs::read_to_string(path)?
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let values: Vec<_> = line.split(',').map(str::trim).collect();
            if values.len() != 4 {
                return Err("expected frame,hex_buttons,stick_x,stick_y".into());
            }
            events.insert(
                values[0].parse::<usize>()?,
                (
                    u16::from_str_radix(values[1], 16)?,
                    values[2].parse::<i8>()?,
                    values[3].parse::<i8>()?,
                ),
            );
        }
    }
    let mut audio = BufWriter::new(fs::File::create(dir.join("audio.s16le"))?);
    let start = Instant::now();
    let mut sample_count = 0usize;
    let mut nonzero = 0usize;
    for frame in 0..frames {
        if let Some(&(buttons, x, y)) = events.get(&frame) {
            engine.set_input(0, buttons, x, y, true);
        }
        engine.tick(true)?;
        let samples = engine.audio();
        for sample in samples {
            audio.write_all(&sample.to_le_bytes())?;
        }
        sample_count += samples.len();
        nonzero += samples.iter().filter(|&&s| s != 0).count();
        if frame % 60 == 0 || frame + 1 == frames {
            let (w, h) = engine.dimensions();
            let mut file =
                BufWriter::new(fs::File::create(dir.join(format!("frame-{frame:06}.ppm")))?);
            write!(file, "P6\n{w} {h}\n255\n")?;
            for pixel in engine.pixels() {
                file.write_all(&pixel.to_le_bytes()[..3])?;
            }
            println!(
                "frame={frame} vi={} pc={:016x} size={w}x{h} elapsed={:.3} samples={sample_count} nonzero={nonzero}",
                engine.vi_count(),
                engine.pc(),
                start.elapsed().as_secs_f64()
            );
        }
    }
    audio.flush()?;
    fs::write(dir.join("state.n64"), engine.save_state()?)?;
    Ok(())
}
