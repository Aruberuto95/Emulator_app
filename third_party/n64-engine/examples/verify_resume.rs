//! Integration check using a locally supplied cartridge, never bundled fixtures.
use std::{fs, time::Instant};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(2..=3).contains(&args.len()) {
        return Err("expected ROM path [initial state]".into());
    }
    let mut engine = n64_engine::Engine::new(fs::read(&args[1])?, 48000)?;
    if let Some(state) = args.get(2) {
        engine.load_state(&fs::read(state)?)?;
    } else {
        for _ in 0..1800 {
            engine.tick(true)?;
        }
    }
    let snapshot = engine.save_state()?;
    println!("snapshot_bytes={}", snapshot.len());
    let mut expected = Vec::new();
    let start = Instant::now();
    for _ in 0..60 {
        engine.tick(true)?;
        expected.push((
            engine.pixels().to_vec(),
            engine.audio().to_vec(),
            engine.cycles(),
        ));
    }
    println!("60_ticks_ms={:.3}", start.elapsed().as_secs_f64() * 1000.0);
    engine.load_state(&snapshot)?;
    for (index, (video, audio, cycles)) in expected.iter().enumerate() {
        engine.tick(true)?;
        assert_eq!(engine.cycles(), *cycles, "CPU diverged at {index}");
        assert_eq!(engine.audio(), audio, "audio diverged at {index}");
        assert!(
            engine.pixels() == video,
            "video diverged at {index}: {} pixels",
            engine
                .pixels()
                .iter()
                .zip(video)
                .filter(|(a, b)| a != b)
                .count()
        );
    }
    let before = engine.cycles();
    let mut damaged = snapshot.clone();
    damaged[90] ^= 1;
    assert!(engine.load_state(&damaged).is_err());
    assert_eq!(engine.cycles(), before, "rejected snapshot mutated CPU");
    assert!(engine.load_state(&snapshot[..snapshot.len() - 1]).is_err());
    assert_eq!(engine.cycles(), before);
    let mut wrong_rom = snapshot.clone();
    wrong_rom[12] ^= 1;
    assert!(engine.load_state(&wrong_rom).is_err());
    assert_eq!(engine.cycles(), before);
    engine.load_state(&snapshot)?;
    for _ in 0..59 {
        engine.tick(false)?;
    }
    engine.tick(true)?;
    assert!(
        engine.pixels() == expected[59].0,
        "skipping presentation changed next frame"
    );
    assert_eq!(
        engine.audio(),
        expected[59].1,
        "skipping presentation changed audio"
    );
    let battery = engine.battery_data()?;
    engine.load_battery(&battery)?;
    assert!(!engine.battery_dirty());
    engine.reset()?;
    for _ in 0..60 {
        engine.tick(true)?;
    }
    println!(
        "PASS: CPU, pixels and audio identical across 60 resumed ticks; corrupt/truncated states rejected; battery and reset passed"
    );
    Ok(())
}
