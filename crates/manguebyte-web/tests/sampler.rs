//! Sampler parts: SFZ instruments loaded a file at a time and played by
//! events, with generated samples like Aratu's (16-bit mono WAV files at 24
//! and 32 kHz).

mod common;

use common::*;
use manguebyte_web::{
    Chain, Engine, EventRecord, InstrumentKind, PANIC_FADE_SECONDS, SAMPLER_ACCENT_DB,
    SAMPLER_LEVEL_DB, SfzLoad, ids,
};

/// An engine with sampler part 0, through no stage, playing `sfz` as it
/// is: not measured for loudness normalisation (`tests/loudness.rs` is
/// about that), so at [`SAMPLER_LEVEL_DB`].
fn sampler_engine(sfz: &str, files: &[(String, Vec<u8>)]) -> Engine {
    let mut engine = engine_with(InstrumentKind::Sampler, Chain::NONE);
    load_sfz_unmeasured(&mut engine, 0, sfz, files);
    engine
}

fn tone_engine() -> Engine {
    let (sfz, files) = tone_instrument();
    sampler_engine(&sfz, &files)
}

/// The frequency of a steady tone: rising zero crossings, placed between
/// samples by straight lines, counted from the first to the last.
fn frequency(samples: &[f32], rate: f64) -> f64 {
    let mut crossings = Vec::new();
    for (index, pair) in samples.windows(2).enumerate() {
        let (a, b) = (f64::from(pair[0]), f64::from(pair[1]));
        if a < 0.0 && b >= 0.0 {
            crossings.push(index as f64 + a / (a - b));
        }
    }
    let (first, last) = (crossings[0], crossings[crossings.len() - 1]);
    (crossings.len() - 1) as f64 / (last - first) * rate
}

fn cents(measured: f64, expected: f64) -> f64 {
    1_200.0 * (measured / expected).log2()
}

/// The largest step between two neighbouring samples.
fn largest_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn every_note_sounds_at_its_pitch() {
    // Both regions, each at its own pitch and above and below it: the
    // 24 kHz A4 and the 32 kHz A5, read at the ratio of their rates to
    // 48 kHz.
    let mut worst: f64 = 0.0;
    for key in [57u8, 64, 69, 72, 74, 75, 81, 84, 93] {
        let mut engine = tone_engine();
        engine.push(EventRecord::note_on(0, 0, key, 1.0));
        let (left, right) = render(&mut engine, 24_000, 128);
        assert_eq!(left, right, "a mono sample in the centre");
        // Past the start, inside the samples and through their loops.
        let measured = frequency(&left[2_000..24_000], RATE);
        let expected = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0);
        let error = cents(measured, expected);
        worst = worst.max(error.abs());
        assert!(
            error.abs() < 0.05,
            "key {key}: {measured} Hz, {error} cents"
        );
    }
    println!("largest pitch error: {worst:.5} cents");
}

#[test]
fn a_continuous_loop_meets_itself_without_a_step() {
    let mut engine = tone_engine();
    engine.push(EventRecord::note_on(0, 0, 69, 1.0));
    // The sample is one second at 24 kHz; four seconds of output go round
    // its loop of 6 000 frames (a quarter of a second at 48 kHz) a dozen
    // times.
    let (left, _) = render(&mut engine, 4 * 48_000, 512);
    let tail = &left[3 * 48_000..];
    assert!(
        peak(tail) > 0.3,
        "still sounding past the end of the sample"
    );
    // Read at half speed, the loop is 12 000 output frames long, and its
    // seam is where the output passes frame 18 000 of the sample.
    let before = largest_step(&left[1_000..24_000]);
    let mut seam: f32 = 0.0;
    let mut at = 36_000;
    while at + 8 < left.len() {
        seam = seam.max(largest_step(&left[at - 8..at + 8]));
        at += 12_000;
    }
    let overall = largest_step(&left[1_000..]);
    println!("step at the loop seam {seam:.6}, before the first loop {before:.6}");
    assert!(seam <= before * 1.000_1, "{seam} against {before}");
    assert!(overall <= before * 1.000_1, "{overall} against {before}");
}

#[test]
fn velocity_and_accent_set_the_level() {
    let peak_of = |velocity: f32, accent: bool| {
        let mut engine = tone_engine();
        engine.push(EventRecord::note_on(0, 0, 69, velocity).with_accent(accent));
        let (left, _) = render(&mut engine, 9_600, 128);
        peak(&left[2_000..])
    };
    let full = peak_of(1.0, false);
    // A sample peaking at 0.8, played at the part's level.
    let level = 0.8 * 10f64.powf(SAMPLER_LEVEL_DB / 20.0);
    assert!((f64::from(full) / level - 1.0).abs() < 0.01, "{full}");
    // The square law of `amp_veltrack=100`, on velocities of 1 to 127.
    let half = peak_of(0.5, false);
    let expected = (64.0f64 / 127.0).powi(2);
    assert!((f64::from(half / full) - expected).abs() < 0.005, "{half}");
    let accented = peak_of(1.0, true);
    let gain_db = 20.0 * f64::from(accented / full).log10();
    println!("accent: {gain_db:.3} dB");
    assert!((gain_db - SAMPLER_ACCENT_DB).abs() < 0.05, "{gain_db}");
    assert!(accented < manguebyte_web::OUTPUT_CEILING);
}

#[test]
fn an_accent_is_for_its_own_note_only() {
    // The same key struck twice, the second time accented while the first
    // still rings in its release: the first is not made louder.
    let first = [
        EventRecord::note_on(0, 0, 69, 0.8),
        EventRecord::note_off(4_800, 0, 69),
    ];
    let second = EventRecord::note_on(4_900, 0, 69, 0.8).with_accent(true);
    let play = |records: &[EventRecord]| {
        let mut engine = tone_engine();
        push_all(&mut engine, records);
        render(&mut engine, 9_600, 128).0
    };
    let both = play(&[first[0], first[1], second]);
    let (first, second) = (play(&first), play(&[second]));
    assert!(peak(&first[4_900..5_400]) > 0.1, "still ringing");
    let apart = both
        .iter()
        .zip(first.iter().zip(&second))
        .map(|(both, (a, b))| (both - (a + b)).abs())
        .fold(0.0, f32::max);
    assert!(apart < 1e-6, "{apart}");
}

#[test]
fn note_off_choke_and_all_notes_off_end_notes() {
    let ended = |stop: EventRecord, within: usize| {
        let mut engine = tone_engine();
        engine.push(EventRecord::note_on(0, 0, 69, 1.0));
        engine.push(stop);
        let (left, _) = render(&mut engine, 4_800 + within + 4_800, 128);
        assert!(peak(&left[2_000..4_800]) > 0.3);
        assert!(
            left[4_800 + within..].iter().all(|&sample| sample == 0.0),
            "{stop:?}"
        );
        assert_eq!(engine.active_voices(0), Some(0));
    };
    // `ampeg_release=0.05`: 90 dB down in 2 400 frames.
    ended(EventRecord::note_off(4_800, 0, 69), 2_400);
    ended(EventRecord::choke(4_800, 0), 1);
    // The release is longer than the panic's floor, so it is used.
    ended(EventRecord::all_notes_off(4_800, 0), 2_400);
}

#[test]
fn a_closed_hat_cuts_the_open_one_off() {
    let (sfz, files) = kit_instrument();
    let play = |records: &[EventRecord]| {
        let mut engine = sampler_engine(&sfz, &files);
        push_all(&mut engine, records);
        render(&mut engine, 24_000, 128).0
    };
    let open = EventRecord::note_on(0, 0, 46, 1.0);
    let closed = EventRecord::note_on(9_600, 0, 42, 1.0);
    let both = play(&[open, closed]);
    let closed_alone = play(&[closed]);
    let open_alone = play(&[open]);
    assert!(
        peak(&open_alone[16_000..]) > 0.05,
        "the open hat rings on alone"
    );
    // The open hat fades out over 6 ms (`off_mode=fast`): after that, only
    // the closed one is left.
    let after = 9_600 + 288 + 1;
    let left_over = both[after..]
        .iter()
        .zip(&closed_alone[after..])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(left_over < 1e-6, "{left_over}");
    // It fades rather than stops.
    assert_ne!(both[9_700], closed_alone[9_700]);
}

#[test]
fn one_shots_ignore_note_off_and_fade_on_all_notes_off() {
    let (sfz, files) = kit_instrument();
    let mut engine = sampler_engine(&sfz, &files);
    engine.push(EventRecord::note_on(0, 0, 46, 1.0));
    engine.push(EventRecord::note_off(100, 0, 46));
    engine.push(EventRecord::all_notes_off(9_600, 0));
    let (left, _) = render(&mut engine, 19_200, 128);
    assert!(
        peak(&left[9_000..9_600]) > 0.05,
        "a note off does not end it"
    );
    let fade = (PANIC_FADE_SECONDS * RATE).round() as usize;
    assert!(left[9_600 + fade..].iter().all(|&sample| sample == 0.0));
    assert_eq!(engine.active_voices(0), Some(0));
}

/// Notes, accents, releases, a choke and an all-notes-off on a sampler
/// part, with a synth and the drums beside it.
fn scenario() -> Engine {
    let (sfz, files) = tone_instrument();
    let (kit_sfz, kit_files) = kit_instrument();
    let mut engine = engine();
    engine.create_part(0, InstrumentKind::Sampler).unwrap();
    engine.create_part(1, InstrumentKind::SamplerKit).unwrap();
    engine.create_part(2, InstrumentKind::Synth).unwrap();
    engine.create_part(3, InstrumentKind::Drums).unwrap();
    load_sfz(&mut engine, 0, &sfz, &files);
    load_sfz(&mut engine, 1, &kit_sfz, &kit_files);
    let mut records = vec![
        EventRecord::param(0, 0, id(ids::REVERB_MIX), 0.3),
        EventRecord::param(0, 1, id(ids::SAMPLER_INTERPOLATION), 1.0),
        EventRecord::param(0, 0, id(ids::GAIN), -6.0),
    ];
    for step in 0..24u64 {
        let frame = step * 1_777 + 5;
        let key = [57u8, 64, 69, 72, 76, 81, 84][step as usize % 7];
        records.push(EventRecord::note_on(frame, 0, key, 0.6).with_accent(step % 3 == 0));
        records.push(EventRecord::note_off(frame + 1_500, 0, key));
        // Sampled, and from the kit: the tap and a tom.
        let drum = [36u8, 42, 88, 38, 46, 50][step as usize % 6];
        records.push(EventRecord::note_on(frame + 3, 1, drum, 0.9).with_accent(step % 5 == 0));
        records.push(EventRecord::note_on(
            frame + 11,
            2,
            45 + (step % 5) as u8,
            0.5,
        ));
        records.push(EventRecord::note_off(frame + 900, 2, 45 + (step % 5) as u8));
        records.push(EventRecord::note_on(frame + 17, 3, 36, 0.8));
    }
    records.extend([
        EventRecord::note_on(20_000, 0, 60, 1.0),
        EventRecord::note_on(20_000, 0, 64, 1.0),
        EventRecord::choke(23_000, 0),
        EventRecord::note_on(30_000, 0, 67, 1.0),
        EventRecord::all_notes_off(33_333, 0),
        EventRecord::all_notes_off(33_333, 1),
    ]);
    push_all(&mut engine, &records);
    engine
}

#[test]
fn the_quanta_do_not_change_the_samples() {
    let frames = 60_000;
    let reference = render(&mut scenario(), frames, 1);
    assert!(peak(&reference.0) > 0.1);
    for quantum in [7, 128, 512] {
        let output = render(&mut scenario(), frames, quantum);
        assert_eq!(bits(&output.0), bits(&reference.0), "left, {quantum}");
        assert_eq!(bits(&output.1), bits(&reference.1), "right, {quantum}");
    }
}

#[test]
fn a_new_instrument_leaves_the_sounding_notes_alone() {
    let (sfz, files) = tone_instrument();
    let (kit_sfz, kit_files) = kit_instrument();
    let start = |engine: &mut Engine| {
        engine.push(EventRecord::note_on(0, 0, 69, 1.0));
        engine.push(EventRecord::note_on(0, 0, 72, 1.0));
    };
    let mut steady = sampler_engine(&sfz, &files);
    let mut swapped = sampler_engine(&sfz, &files);
    start(&mut steady);
    start(&mut swapped);
    let (a, _) = render(&mut steady, 6_000, 128);
    let (b, _) = render(&mut swapped, 6_000, 128);
    assert_eq!(bits(&a), bits(&b));
    // The kit takes over in the middle of the notes: they ring on as they
    // were, and the next note plays the kit. (Measured, the kit would bring
    // its own make-up gain, which the part would glide to.)
    load_sfz_unmeasured(&mut swapped, 0, &kit_sfz, &kit_files);
    let (a, _) = render(&mut steady, 12_000, 128);
    let (b, _) = render(&mut swapped, 12_000, 128);
    assert_eq!(bits(&a), bits(&b), "the sounding notes did not change");
    swapped.push(EventRecord::note_off(swapped.frame(), 0, 69));
    swapped.push(EventRecord::note_off(swapped.frame(), 0, 72));
    render(&mut swapped, 4_800, 128);
    assert_eq!(swapped.active_voices(0), Some(0));
    assert_eq!(swapped.collect_instruments(), 1, "the old one is freed");
    swapped.push(EventRecord::note_on(swapped.frame(), 0, 36, 1.0));
    swapped.push(EventRecord::note_on(swapped.frame(), 0, 69, 1.0));
    render(&mut swapped, 128, 128);
    assert_eq!(
        swapped.active_voices(0),
        Some(1),
        "only the kick: no key 69"
    );
}

#[test]
fn a_sampler_part_is_silent_until_it_has_an_instrument() {
    let mut engine = engine_with(InstrumentKind::Sampler, Chain::FULL);
    engine.push(EventRecord::note_on(0, 0, 60, 1.0));
    let (left, _) = render(&mut engine, 1_000, 128);
    assert!(left.iter().all(|&sample| sample == 0.0));
    assert_eq!(engine.active_voices(0), Some(0));
    let json = engine.params_json(0).unwrap();
    assert!(json.contains("\"sampler.interpolation\""), "{json}");
}

#[test]
fn only_sampler_parts_take_instruments() {
    let (sfz, files) = tone_instrument();
    let mut load = SfzLoad::new(&sfz).unwrap();
    for (path, bytes) in &files {
        load.add_file(path, bytes.clone()).unwrap();
    }
    let instrument = load.finish().unwrap();
    let mut engine = engine_with(InstrumentKind::Synth, Chain::NONE);
    assert_eq!(
        engine.load_instrument(0, instrument).unwrap_err(),
        manguebyte_web::EngineError::NotASampler(0)
    );
}

#[test]
fn loads_say_what_is_wrong() {
    let error = SfzLoad::new("<region> sample=a.wav lokey=x\n<regi").unwrap_err();
    assert!(error.to_string().contains("line 2"), "{error}");
    assert!(SfzLoad::new("#include \"other.sfz\"").is_err());

    let (sfz, files) = tone_instrument();
    let mut load = SfzLoad::new(&sfz).unwrap();
    assert_eq!(load.missing_json(), "[\"a4.wav\",\"a5.wav\"]");
    assert!(!load.add_file("elsewhere.wav", vec![1, 2, 3]).unwrap());
    let error = load
        .add_file("a4.wav", b"RIFF1234WAVEjunk".to_vec())
        .unwrap_err();
    assert!(error.to_string().contains("a4.wav"), "{error}");

    let mut load = SfzLoad::new(&sfz).unwrap();
    load.add_file("a4.wav", files[0].1.clone()).unwrap();
    assert_eq!(load.missing().collect::<Vec<_>>(), ["a5.wav"]);
    let error = load.finish().unwrap_err();
    assert!(error.to_string().contains("a5.wav"), "{error}");
}

/// A sampler kit with the fixture kit, as it is, or with no instrument.
fn kit_engine(loaded: bool) -> Engine {
    let mut engine = engine_with(InstrumentKind::SamplerKit, Chain::FULL);
    if loaded {
        let (sfz, files) = kit_instrument();
        load_sfz_unmeasured(&mut engine, 0, &sfz, &files);
    }
    engine
}

/// What the synthesized drum kit plays for `records`, through the full
/// chain.
fn drums_alone(records: &[EventRecord]) -> (Vec<f32>, Vec<f32>) {
    let mut engine = engine_with(InstrumentKind::Drums, Chain::FULL);
    push_all(&mut engine, records);
    render(&mut engine, 24_000, 128)
}

#[test]
fn a_sampler_kit_plays_the_synthesized_drum_its_sfz_lacks() {
    // The fixture kit, like Aratu's, has no guitar tap (88) and no toms:
    // those come from the drum kit, at their velocity, with their accent,
    // through the part's chain and gain.
    let records = [
        EventRecord::param(0, 0, id(ids::GAIN), -4.0),
        EventRecord::param(0, 0, id(ids::REVERB_MIX), 0.4),
        EventRecord::note_on(100, 0, 88, 0.7).with_accent(true),
        EventRecord::note_on(5_000, 0, 50, 0.9),
        EventRecord::note_on(9_000, 0, 43, 0.5).with_accent(true),
    ];
    let mut engine = kit_engine(true);
    push_all(&mut engine, &records);
    let (left, right) = render(&mut engine, 24_000, 128);
    let (drum_left, drum_right) = drums_alone(&records);
    assert!(peak(&left) > 0.05);
    assert_eq!(bits(&left), bits(&drum_left));
    assert_eq!(bits(&right), bits(&drum_right));
}

#[test]
fn a_sampler_kit_plays_its_samples_for_the_notes_it_has() {
    // A kick and a snare from the samples: the same as a sampler part that
    // is not a kit, and not the synthesized ones.
    let records = [
        EventRecord::note_on(100, 0, 36, 0.9).with_accent(true),
        EventRecord::note_on(4_000, 0, 38, 0.6),
    ];
    let (sfz, files) = kit_instrument();
    let mut plain = engine_with(InstrumentKind::Sampler, Chain::FULL);
    load_sfz_unmeasured(&mut plain, 0, &sfz, &files);
    push_all(&mut plain, &records);
    let mut engine = kit_engine(true);
    push_all(&mut engine, &records);
    let (left, _) = render(&mut engine, 24_000, 128);
    let (sampled, _) = render(&mut plain, 24_000, 128);
    assert_eq!(bits(&left), bits(&sampled));
    let (drums, _) = drums_alone(&records);
    assert_ne!(bits(&left), bits(&drums));
}

#[test]
fn a_sampler_kit_is_the_drum_kit_until_it_has_samples() {
    let records: Vec<EventRecord> = manguebyte_web::DRUM_MAP
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            EventRecord::note_on(index as u64 * 1_500, 0, entry.note, 0.8)
                .with_accent(index % 2 == 0)
        })
        .collect();
    let mut engine = kit_engine(false);
    push_all(&mut engine, &records);
    let (left, right) = render(&mut engine, 24_000, 128);
    let (drum_left, drum_right) = drums_alone(&records);
    assert_eq!(bits(&left), bits(&drum_left));
    assert_eq!(bits(&right), bits(&drum_right));
}

#[test]
fn only_a_sampler_kit_falls_back_and_only_on_drum_notes() {
    // A melodic sampler part plays nothing for a key it has no region for,
    // drum map or not; a kit plays nothing for a key outside the drum map.
    let (kit_sfz, kit_files) = kit_instrument();
    let mut melodic = engine_with(InstrumentKind::Sampler, Chain::NONE);
    load_sfz(&mut melodic, 0, &kit_sfz, &kit_files);
    melodic.push(EventRecord::note_on(0, 0, 88, 1.0));
    melodic.push(EventRecord::note_on(0, 0, 50, 1.0));
    let (left, _) = render(&mut melodic, 4_800, 128);
    assert!(left.iter().all(|&sample| sample == 0.0));
    let mut kit = kit_engine(true);
    kit.push(EventRecord::note_on(0, 0, 60, 1.0));
    let (left, _) = render(&mut kit, 4_800, 128);
    assert!(left.iter().all(|&sample| sample == 0.0));
}

#[test]
fn a_sampler_kit_has_the_kit_parameters_for_its_drums() {
    let records = [
        EventRecord::param(0, 0, id(ids::GUITAR_TAP_LEVEL), 0.3),
        EventRecord::param(0, 0, id(ids::KIT_TUNE), -5.0),
        EventRecord::note_on(10, 0, 88, 1.0),
    ];
    let mut engine = kit_engine(true);
    push_all(&mut engine, &records);
    let (left, _) = render(&mut engine, 24_000, 128);
    assert_eq!(bits(&left), bits(&drums_alone(&records).0));
    let level = engine.param_value(0, ids::GUITAR_TAP_LEVEL).unwrap();
    assert!((level - 0.3).abs() < 1e-6, "{level}");
    assert_eq!(engine.part_kind(0), Some(InstrumentKind::SamplerKit));
    // Choke and all notes off reach both halves.
    for stop in [
        EventRecord::choke(2_000, 0),
        EventRecord::all_notes_off(2_000, 0),
    ] {
        let mut engine = kit_engine(true);
        push_all(
            &mut engine,
            &[
                EventRecord::note_on(0, 0, 88, 1.0),
                EventRecord::note_on(0, 0, 46, 1.0),
                stop,
            ],
        );
        // The parts run the master's look-ahead, 111 frames, ahead.
        render(&mut engine, 1_800, 128);
        assert_eq!(engine.active_voices(0), Some(2), "{stop:?}");
        render(&mut engine, 2_400 + 200, 128);
        assert_eq!(engine.active_voices(0), Some(0), "{stop:?}");
    }
}
