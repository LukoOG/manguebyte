//! The master bus: the limiter keeps a loud set under its ceiling, between
//! the samples too; the glue compressor and the shared reverb do what they
//! say; and the bus's latency is what the engine reports.

mod common;

use common::*;
use manguebyte_analysis::TruePeakMeter;
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::ProcessSpec;
use manguebyte_web::{
    Chain, Drum, Effect, EngineConfig, EventRecord, InstrumentKind, OUTPUT_CEILING, Visual, ids,
    master_latency_frames,
};

/// The true peak of `left` and `right`, in dBTP, read by a meter that
/// interpolates by sixteen.
fn true_peak_db(left: &[f32], right: &[f32]) -> f64 {
    let mut meter = TruePeakMeter::new(2).with_oversampling(16);
    meter.prepare(&ProcessSpec {
        sample_rate: RATE,
        max_block_size: left.len(),
        num_channels: 2,
    });
    let mut buffer = AudioBuffer::<f32>::new(2, left.len());
    buffer.channel_mut(0).copy_from_slice(left);
    buffer.channel_mut(1).copy_from_slice(right);
    meter.measure(&buffer);
    meter.flush();
    meter.readings().max_overall_db()
}

/// Every kind of part, loud: chords on two synths with their gain up, two
/// drum kits hammering every drum, accented, and their reverbs and sends.
fn loud_set(ceiling: Option<f64>) -> (Vec<f32>, Vec<f32>, u64) {
    let mut engine = engine();
    for (part, kind) in [
        (0, InstrumentKind::Synth),
        (1, InstrumentKind::Synth),
        (2, InstrumentKind::Drums),
        (3, InstrumentKind::Drums),
    ] {
        engine.create_part_with(part, kind, Chain::ALL).unwrap();
    }
    let mut records = Vec::new();
    if let Some(ceiling) = ceiling {
        records.push(EventRecord::param(0, 0, id(ids::LIMITER_CEILING), ceiling));
    }
    for part in 0..4u16 {
        records.push(EventRecord::param(0, part, id(ids::GAIN), 9.0));
        records.push(EventRecord::param(0, part, id(ids::SEND), 0.5));
        // Bright and hard: drive and a high shelf, which put the most
        // between the samples.
        records.push(EventRecord::param(0, part, id(ids::DRIVE_ON), 1.0));
        records.push(EventRecord::param(0, part, id(ids::EQ_ON), 1.0));
        records.push(EventRecord::param(0, part, id(ids::EQ_HIGH_GAIN), 12.0));
    }
    for (part, notes) in [(0u16, [36u8, 43, 48, 52]), (1, [60, 64, 67, 71])] {
        records.push(EventRecord::param(0, part, id(ids::SYNTH_WAVE), 1.0));
        for note in notes {
            records.push(EventRecord::note_on(0, part, note, 1.0));
        }
    }
    for hit in 0..24u64 {
        for part in [2u16, 3] {
            let drum = Drum::ALL[(hit as usize + usize::from(part)) % Drum::ALL.len()];
            records
                .push(EventRecord::note_on(hit * 2_000, part, drum.note(), 1.0).with_accent(true));
        }
    }
    push_all(&mut engine, &records);
    let (left, right) = render(&mut engine, 48_000, 128);
    (left, right, engine.stats().limited)
}

#[test]
fn the_limiter_keeps_a_loud_set_under_its_ceiling_between_the_samples_too() {
    for ceiling in [None, Some(-1.0), Some(-6.0)] {
        let (left, right, limited) = loud_set(ceiling);
        let wanted = ceiling.unwrap_or(-0.3);
        let sample_peak = 20.0 * f64::from(peak(&left).max(peak(&right))).log10();
        let true_peak = true_peak_db(&left, &right);
        eprintln!(
            "ceiling {wanted} dBTP: sample peak {sample_peak:.3} dB, true peak {true_peak:.3} dBTP, {limited} frames limited"
        );
        assert!(limited > 10_000, "the set is not loud enough: {limited}");
        assert!(
            left.iter()
                .chain(&right)
                .all(|s| s.is_finite() && s.abs() <= OUTPUT_CEILING)
        );
        // Samples under the ceiling; the waveform between them within what
        // reading it eight times oversampled, below 0.44 of the rate, can miss.
        assert!(sample_peak <= wanted + 1e-4, "{sample_peak}");
        assert!(true_peak <= wanted + 0.15, "{true_peak}");
        assert!(
            true_peak > wanted - 1.0,
            "held far under the ceiling: {true_peak}"
        );
    }
}

#[test]
fn below_the_ceiling_the_bus_leaves_the_sound_exactly_as_it_is() {
    // A sine at -12 dBFS, and the same through a master gain of -6 dB.
    let tone = sine(440.0, RATE, 48_000, 0.5);
    let run = |gain: f64| {
        let mut engine = signal_engine(&tone, Chain::NONE);
        push_all(
            &mut engine,
            &[
                EventRecord::param(0, 0, id(ids::MASTER_GAIN), gain),
                EventRecord::note_on(0, 0, 60, 1.0),
            ],
        );
        let (left, _) = render(&mut engine, 30_000, 128);
        assert_eq!(engine.stats().limited, 0);
        left
    };
    let plain = run(0.0);
    let level = sampler_level();
    for (n, &sample) in plain.iter().enumerate().take(30_000) {
        assert_eq!(sample, tone[n] * level, "frame {n}");
    }
    // The master gain glides there over 20 ms, then holds.
    let quieter = run(-6.0);
    let gain = 10f64.powf(-6.0 / 20.0);
    for (&a, &b) in plain.iter().zip(&quieter).skip(1_000) {
        assert!((f64::from(b) - f64::from(a) * gain).abs() < 1e-6);
    }
}

#[test]
fn the_glue_compressor_reduces_by_its_ratio_and_matches_its_curve() {
    // A square wave of steady level, -6 dBFS through the part.
    let square: Vec<f32> = (0..48_000)
        .map(|n| if (n / 240) % 2 == 0 { 1.0 } else { -1.0 })
        .collect();
    let settings = [
        (ids::GLUE_THRESHOLD, -12.0),
        (ids::GLUE_RATIO, 2.0),
        (ids::GLUE_KNEE, 0.0),
        (ids::GLUE_ATTACK, 1.0),
        (ids::GLUE_MAKEUP, 2.0),
    ];
    let mut engine = signal_engine(&square, Chain::NONE);
    let mut records: Vec<EventRecord> = settings
        .iter()
        .map(|&(name, value)| EventRecord::param(0, 0, id(name), value))
        .collect();
    records.push(EventRecord::param(0, 0, id(ids::GLUE_ON), 1.0));
    records.push(EventRecord::note_on(0, 0, 60, 1.0));
    push_all(&mut engine, &records);
    let (left, _) = render(&mut engine, 30_000, 128);
    assert!(engine.glue_running());
    let input = 20.0 * f64::from(sampler_level()).log10();
    let output = 20.0 * f64::from(peak(&left[20_000..30_000])).log10();
    // 6 dB over at 2:1 leaves 3 dB, and 2 dB of make-up.
    let expected = -12.0 + (input + 12.0) / 2.0 + 2.0;
    eprintln!("glue: {input:.2} dBFS in, {output:.2} out, {expected:.2} expected");
    assert!((output - expected).abs() < 0.1, "{output} {expected}");
    let curve = engine.master_curve(Effect::Glue, 661).unwrap();
    assert_eq!(curve.visual, Visual::TransferCurve);
    let drawn = curve
        .points
        .iter()
        .min_by(|a, b| {
            (f64::from(a[0]) - input)
                .abs()
                .total_cmp(&(f64::from(b[0]) - input).abs())
        })
        .unwrap();
    assert!((f64::from(drawn[1]) - expected).abs() < 0.1, "{drawn:?}");
}

#[test]
fn the_shared_reverb_rings_on_what_parts_send_and_sleeps_after() {
    let mut click = vec![0.0f32; 480];
    click[0] = 1.0;
    let run = |send: f64| {
        let mut engine = signal_engine(&click, Chain::NONE);
        push_all(
            &mut engine,
            &[
                EventRecord::param(0, 0, id(ids::SEND), send),
                EventRecord::param(0, 0, id(ids::MASTER_REVERB_DECAY), 0.5),
                EventRecord::note_on(0, 0, 60, 1.0),
            ],
        );
        let (left, right) = render(&mut engine, 4_800, 128);
        let awake = !engine.master_reverb_asleep();
        render(&mut engine, 96_000, 128);
        (left, right, awake, engine.master_reverb_asleep())
    };
    let (dry, _, awake, _) = run(0.0);
    assert!(!awake, "nothing sent: the reverb never wakes");
    assert!(dry[1..].iter().all(|&sample| sample == 0.0));
    let (wet, wet_right, awake, asleep_later) = run(1.0);
    assert!(awake);
    assert_eq!(wet[0], dry[0], "the dry click is the same");
    assert!(peak(&wet[1_000..4_800]) > 1e-3, "a tail");
    assert!(wet[1_000..4_800] != wet_right[1_000..4_800], "a wide one");
    assert!(
        asleep_later,
        "and the reverb falls asleep once it has died away"
    );
    let decay = engine().master_curve(Effect::Reverb, 31).unwrap();
    assert_eq!(decay.visual, Visual::ReverbDecay);
    assert_eq!(decay.points[0], [0.0, 0.0]);
}

#[test]
fn the_bus_is_late_by_its_look_ahead_and_the_engine_counts_it() {
    for rate in [
        EngineConfig::MIN_SAMPLE_RATE,
        44_100.0,
        48_000.0,
        96_000.0,
        EngineConfig::MAX_SAMPLE_RATE,
    ] {
        let mut engine = manguebyte_web::Engine::new(EngineConfig::new(rate)).unwrap();
        let ahead = master_latency_frames(rate);
        let expected = manguebyte_web::TRUE_PEAK_DELAY + ((0.002 * rate).round() as usize).max(1) - 1;
        assert_eq!(ahead, expected, "{rate}");
        assert_eq!(manguebyte_web::TRUE_PEAK_DELAY, 16);
        assert_eq!(engine.master_latency_frames(), ahead);
        engine.render(64);
        assert_eq!(engine.latency_frames(), 64 + ahead);
    }
    assert_eq!(master_latency_frames(48_000.0), 111);
    // The limiter's curve: a wall at the ceiling, which is never above
    // the output ceiling.
    let wall = engine().master_curve(Effect::Limiter, 67).unwrap();
    let highest = 20.0 * f64::from(OUTPUT_CEILING).log10();
    for &[x, y] in &wall.points {
        assert_eq!(y, f64::from(x).min(highest) as f32);
    }
    assert_eq!(engine().master_curve(Effect::Delay, 8), None);
}

#[test]
fn synced_effects_follow_the_beat_they_are_given() {
    // A tremolo of a whole cycle a beat: setting the beat to half a beat
    // on frame 0 puts it half a cycle on.
    let steady = vec![1.0f32; 48_000];
    let run = |beat: f64| {
        let mut engine = signal_engine(&steady, Chain::NONE.with(Effect::Tremolo));
        push_all(
            &mut engine,
            &[
                EventRecord::param(0, 0, id(ids::MASTER_BEAT), beat),
                EventRecord::param(0, 0, id(ids::TREMOLO_SYNC), 1.0),
                EventRecord::param(0, 0, id(ids::TREMOLO_DEPTH), 1.0),
                EventRecord::param(0, 0, id(ids::TREMOLO_ON), 1.0),
                EventRecord::note_on(0, 0, 60, 1.0),
            ],
        );
        render(&mut engine, 40_000, 128).0
    };
    let (on_the_beat, half_on) = (run(0.0), run(0.5));
    // At 120 beats a minute a beat is 24 000 frames.
    for n in (2_000..16_000).step_by(1_000) {
        assert!(
            (on_the_beat[n] - half_on[n + 12_000]).abs() < 1e-3,
            "{n}: {} {}",
            on_the_beat[n],
            half_on[n + 12_000]
        );
    }
}
