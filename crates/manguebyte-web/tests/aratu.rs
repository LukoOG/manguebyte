//! Aratu's own instruments, when they are on this machine: every voice
//! loads with no warning, file by file, and plays; the test prints the
//! make-up gain its loudness normalisation measured for each.
//!
//! The samples are Aratu's (`web/public/samples/<voice>/voice.sfz` and its
//! WAV files, credited in Aratu's `samples/SOURCES.md`) and are not copied
//! here. The test reads them from the directory in `ARATU_SAMPLES`, or from
//! `C:\aratu\web\public\samples`, and passes, saying so, when neither is
//! there, as on CI.

mod common;

use std::path::{Path, PathBuf};
use std::time::Instant;

use common::*;
use manguebyte_web::{Chain, EventRecord, InstrumentKind, OUTPUT_CEILING, SfzLoad};

fn samples_directory() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("ARATU_SAMPLES").map(PathBuf::from),
        Some(PathBuf::from(r"C:\aratu\web\public\samples")),
    ];
    candidates.into_iter().flatten().find(|path| path.is_dir())
}

/// Every voice directory, with its SFZ text.
fn voices(directory: &Path) -> Vec<(String, PathBuf, String)> {
    let mut voices: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let text = std::fs::read_to_string(path.join("voice.sfz")).ok()?;
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some((name, path, text))
        })
        .collect();
    voices.sort();
    voices
}

#[test]
fn every_aratu_voice_loads_and_plays() {
    let Some(directory) = samples_directory() else {
        println!("Aratu's samples are not here: set ARATU_SAMPLES to test them");
        return;
    };
    let voices = voices(&directory);
    assert!(
        !voices.is_empty(),
        "no voice.sfz under {}",
        directory.display()
    );
    for (name, path, text) in voices {
        let mut load = SfzLoad::new(&text).unwrap_or_else(|error| panic!("{name}: {error}"));
        let warnings: Vec<String> = load
            .sfz()
            .warnings
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(warnings.is_empty(), "{name}: {warnings:?}");
        let keys: Vec<u8> = load
            .sfz()
            .regions
            .iter()
            .filter_map(|region| match region.pitch_keycenter {
                manguebyte_web::manguebyte_sampler::KeyCenter::Key(key)
                    if region.lokey != region.hikey =>
                {
                    Some(key)
                }
                _ => u8::try_from(region.lokey).ok(),
            })
            .collect();
        let single_keys = load
            .sfz()
            .regions
            .iter()
            .all(|region| region.lokey == region.hikey);
        let files: Vec<String> = load.paths().map(str::to_owned).collect();
        let mut slowest = 0.0f64;
        let mut bytes_total = 0;
        for file in &files {
            let bytes = std::fs::read(path.join(file)).unwrap();
            bytes_total += bytes.len();
            let start = Instant::now();
            load.add_file(file, bytes)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            slowest = slowest.max(start.elapsed().as_secs_f64());
        }
        let instrument = load.finish().unwrap();
        println!(
            "{name}: {} regions, {} files, {} kB, slowest file {:.2} ms (this build)",
            instrument.num_regions(),
            files.len(),
            bytes_total / 1_000,
            slowest * 1e3
        );
        // A kit: every region on a key of its own. It is played as a
        // sampler kit, which takes the drums it has no sample for from the
        // synthesized kit.
        let is_kit = single_keys;
        let kind = if is_kit {
            InstrumentKind::SamplerKit
        } else {
            InstrumentKind::Sampler
        };
        let mut engine = engine_with(kind, Chain::NONE);
        engine.load_instrument(0, instrument).unwrap();
        // What the loudness normalisation measured: how far each voice is
        // from the target as Aratu recorded it.
        match engine.sampler_make_up_db(0) {
            Some(make_up) => println!("{name}: normalised by {make_up:+.2} dB"),
            None => println!("{name}: not measured, it plays no key"),
        }
        if is_kit {
            let missing: Vec<u8> = manguebyte_web::DRUM_MAP
                .iter()
                .map(|entry| entry.note)
                .filter(|note| !keys.contains(note))
                .collect();
            println!("{name}: the kit plays {missing:?}");
            for note in missing {
                engine.push(EventRecord::note_on(engine.frame(), 0, note, 0.9));
                let (left, _) = render(&mut engine, 9_600, 128);
                assert!(peak(&left) > 0.01, "{name}: drum {note} is silent");
                render(&mut engine, 48_000, 128);
            }
        }
        for &key in &keys {
            engine.push(EventRecord::note_on(engine.frame(), 0, key, 0.9));
            let (left, right) = render(&mut engine, 9_600, 128);
            let loudest = peak(&left).max(peak(&right));
            assert!(loudest > 0.01, "{name}: key {key} is silent");
            assert!(loudest < OUTPUT_CEILING, "{name}: key {key}");
            assert!(left.iter().all(|sample| sample.is_finite()));
            engine.push(EventRecord::all_notes_off(engine.frame(), 0));
            render(&mut engine, 48_000, 128);
            assert_eq!(engine.active_voices(0), Some(0), "{name}: key {key}");
        }
        // Looped voices go on sounding while the key is held.
        if text.contains("loop_continuous") {
            let key = keys[keys.len() / 2];
            engine.push(EventRecord::note_on(engine.frame(), 0, key, 0.9));
            let (left, _) = render(&mut engine, 6 * 48_000, 512);
            assert!(peak(&left[5 * 48_000..]) > 0.01, "{name}: the loop stopped");
        }
    }
}
