use manguebyte_midi::MidiMessage;
use manguebyte_midi::file::{MetaEvent, MidiFile, Track};

use super::*;

const RATES: [f64; 3] = [44_100.0, 48_000.0, 96_000.0];

/// How many frames the long tests walk through; fewer under Miri.
const SPAN: i64 = if cfg!(miri) { 2_000 } else { 600_000 };

/// A frame every so often across [`SPAN`], with the frames either side.
fn frames() -> impl Iterator<Item = i64> {
    let step = if cfg!(miri) { 101 } else { 997 };
    (-3 * step..SPAN).step_by(step as usize)
}

fn four_four() -> TimeSignature {
    TimeSignature::FOUR_FOUR
}

/// 120 bpm; a ramp up to 150 over beats 8 to 16; 150 held; a ramp down
/// from 97.5 to 60 over beats 20 to 23; then 60. Bars 0 to 3 in 4/4, bars
/// 4 and 5 in 7/8, 3/4 from bar 6 (beat 23).
fn song() -> TempoMap {
    let mut map = TempoMap::new(120.0, four_four()).unwrap();
    map.set_tempo(8.0, 120.0, TempoCurve::Linear).unwrap();
    map.set_tempo(16.0, 150.0, TempoCurve::Step).unwrap();
    map.set_tempo(20.0, 97.5, TempoCurve::Linear).unwrap();
    map.set_tempo(23.0, 60.0, TempoCurve::Step).unwrap();
    map.set_signature(4, TimeSignature::new(7, 8)).unwrap();
    map.set_signature(6, TimeSignature::new(3, 4)).unwrap();
    map
}

fn close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} is not within {tolerance} of {expected}"
    );
}

// --- The map ----------------------------------------------------------------

#[test]
fn a_steady_tempo_converts_exactly() {
    let map = TempoMap::new(120.0, four_four()).unwrap();
    assert!(map.is_constant());
    for rate in RATES {
        // Half a second per beat: these are exact in binary.
        assert_eq!(map.seconds_at_beat(3.0), 1.5);
        assert_eq!(map.beat_at_seconds(1.5), 3.0);
        assert_eq!(map.sample_at_beat(2.0, rate), rate);
        assert_eq!(map.beat_at_sample(rate, rate), 2.0);
        assert_eq!(map.frame_at_beat(2.0, rate), rate as i64);
        assert_eq!(map.beat_at_frame(rate as i64, rate), 2.0);
        assert_eq!(map.tempo_at_frame(12_345, rate), 120.0);
        let position = map.position_at_frame(rate as i64 * 3, rate);
        assert_eq!(position.beat, 6.0);
        assert_eq!(position.seconds, 3.0);
        assert_eq!(position.bar, 1);
        assert_eq!(position.bar_start_beat, 4.0);
        assert_eq!(position.tempo_increment, 0.0);
    }
    assert_eq!(TempoMap::default(), map);
}

#[test]
fn steps_add_up_segment_by_segment() {
    let mut map = TempoMap::new(120.0, four_four()).unwrap();
    map.set_tempo(4.0, 60.0, TempoCurve::Step).unwrap();
    map.set_tempo(6.0, 240.0, TempoCurve::Step).unwrap();
    // 4 beats at 120, 2 at 60, then 240.
    assert_eq!(map.seconds_at_beat(4.0), 2.0);
    assert_eq!(map.seconds_at_beat(5.0), 3.0);
    assert_eq!(map.seconds_at_beat(6.0), 4.0);
    assert_eq!(map.seconds_at_beat(10.0), 5.0);
    assert_eq!(map.beat_at_seconds(4.5), 8.0);
    assert_eq!(map.tempo_at_beat(3.999), 120.0);
    assert_eq!(map.tempo_at_beat(4.0), 60.0);
    assert_eq!(map.tempo_at_seconds(3.999), 60.0);
    assert_eq!(map.tempo_at_seconds(4.0), 240.0);
    for rate in RATES {
        let change = map.frame_at_beat(4.0, rate);
        assert_eq!(change, 2 * rate as i64);
        assert_eq!(map.tempo_at_frame(change - 1, rate), 120.0);
        assert_eq!(map.tempo_at_frame(change, rate), 60.0);
    }
    // A linear point with nothing after it holds.
    map.set_tempo(10.0, 100.0, TempoCurve::Linear).unwrap();
    assert_eq!(map.tempo_at_beat(1_000.0), 100.0);
    assert_eq!(map.tempo_points().count(), 4);
}

#[test]
fn a_ramp_is_linear_in_time_and_integrates_its_tempo() {
    let map = song();
    let (start, end) = (map.seconds_at_beat(8.0), map.seconds_at_beat(16.0));
    let duration = end - start;
    // D = 120 L / (T0 + T1).
    close(duration, 120.0 * 8.0 / 270.0, 1e-13);

    for step in 0..=100 {
        let t = duration * f64::from(step) / 100.0;
        // The tempo is linear in time...
        close(
            map.tempo_at_seconds(start + t),
            120.0 + 30.0 * t / duration,
            1e-11,
        );
        // ...agrees between the two ways of asking...
        let beat = map.beat_at_seconds(start + t);
        close(
            map.tempo_at_beat(beat),
            map.tempo_at_seconds(start + t),
            1e-9,
        );
        // ...and the beat moves at the tempo: the slope of beats against
        // seconds, by a central difference, is the tempo over 60.
        if step > 0 && step < 100 {
            let h = 1e-5;
            let slope = (map.beat_at_seconds(start + t + h) - map.beat_at_seconds(start + t - h))
                / (2.0 * h);
            close(slope * 60.0, map.tempo_at_seconds(start + t), 1e-6);
        }
    }

    // An independent integration of a tempo that gains the same amount
    // every step reaches the end of the ramp at the time the map says.
    // The midpoint rule is exact for a linear tempo, up to rounding.
    let steps = 10_000;
    let dt = duration / f64::from(steps);
    let slope = 30.0 / duration;
    let mut beats = 0.0;
    for step in 0..steps {
        let t = f64::from(step) * dt;
        let tempo = 120.0 + slope * (t + dt / 2.0);
        beats += tempo / 60.0 * dt;
    }
    close(beats, 8.0, 1e-9);

    // The frame-to-frame increment is the ramp's slope over the rate.
    for rate in RATES {
        let frame = map.frame_at_beat(12.0, rate);
        let position = map.position_at_frame(frame, rate);
        close(position.tempo_increment, slope / rate, 1e-15);
        let next = map.tempo_at_frame(frame + 1, rate);
        close(next - position.tempo_bpm, position.tempo_increment, 1e-9);
        // Outside a ramp it is zero.
        assert_eq!(map.position_at_frame(0, rate).tempo_increment, 0.0);
    }
}

#[test]
fn a_nearly_flat_ramp_stays_exact() {
    let mut flat = TempoMap::new(100.0, four_four()).unwrap();
    flat.set_tempo(4.0, 100.0, TempoCurve::Linear).unwrap();
    flat.set_tempo(8.0, 100.0 * (1.0 + 1e-12), TempoCurve::Step)
        .unwrap();
    let steady = TempoMap::new(100.0, four_four()).unwrap();
    for beat in [4.0, 5.0, 6.5, 7.999] {
        // The ramp itself shortens 4 beats by under 2.4e-12 s; a formula
        // that cancelled would be off by far more.
        let seconds = flat.seconds_at_beat(beat);
        let gap = steady.seconds_at_beat(beat) - seconds;
        assert!((0.0..2.5e-12).contains(&gap), "{gap}");
        close(flat.beat_at_seconds(seconds), beat, 1e-14);
    }
}

#[test]
fn seconds_and_beats_round_trip_across_steps_and_ramps() {
    let map = song();
    let mut beat = -8.0;
    while beat < 40.0 {
        let seconds = map.seconds_at_beat(beat);
        close(
            map.beat_at_seconds(seconds),
            beat,
            1e-12 * beat.abs().max(1.0),
        );
        let back = map.seconds_at_beat(map.beat_at_seconds(seconds));
        close(back, seconds, 1e-12 * seconds.abs().max(1.0));
        beat += 0.123;
    }
}

#[test]
fn frames_beats_and_bars_round_trip_at_every_rate() {
    let map = song();
    for rate in RATES {
        let mut last_beat = f64::NEG_INFINITY;
        for frame in frames() {
            // Frame to beat and back is exact.
            let beat = map.beat_at_frame(frame, rate);
            assert_eq!(
                map.frame_at_beat(beat, rate),
                frame,
                "frame {frame} at {rate}"
            );
            assert!(beat > last_beat, "beats go forward");
            last_beat = beat;

            // Beat to bars, beats and ticks and back loses less than a
            // tick, and never goes past the beat.
            let bbt = map.bbt_at_beat(beat, 960);
            let back = map.beat_at_bbt(bbt, 960);
            let tick = map.signature_at_beat(beat).beat_length() / 960.0;
            assert!(back <= beat + 1e-9, "{bbt} is after beat {beat}");
            assert!(
                beat - back < tick,
                "{bbt} is a tick or more before beat {beat}"
            );

            // The bar a frame reports is the one its beat falls in, or
            // the next when the bar line lands on this very frame.
            let position = map.position_at_frame(frame, rate);
            assert_eq!(position.beat, beat);
            assert_eq!(position.tempo_bpm, map.tempo_at_frame(frame, rate));
            let line = map.frame_at_beat(map.beat_at_bar(position.bar), rate);
            let next = map.frame_at_beat(map.beat_at_bar(position.bar + 1), rate);
            assert!(
                line <= frame && frame < next,
                "frame {frame} is not in bar {}",
                position.bar
            );
            assert_eq!(position.bar_start_beat, map.beat_at_bar(position.bar));
            assert_eq!(position.signature, map.signature_at_bar(position.bar));
        }
    }
}

#[test]
fn bars_beats_and_ticks_round_trip_exactly() {
    let map = song();
    for ticks_per_beat in [1, 3, 96, 960] {
        for bar in -2..12 {
            let signature = map.signature_at_bar(bar);
            for beat in 0..u32::from(signature.numerator) {
                for tick in [0, ticks_per_beat / 3, ticks_per_beat - 1] {
                    let bbt = BarBeatTick { bar, beat, tick };
                    let at = map.beat_at_bbt(bbt, ticks_per_beat);
                    assert_eq!(map.bbt_at_beat(at, ticks_per_beat), bbt);
                    // And through frames: the frame it lands on is less than
                    // a frame after it, or a ten-thousandth of one before.
                    for rate in RATES {
                        let frame = map.frame_at_beat(at, rate);
                        let after = map.beat_at_frame(frame, rate) - at;
                        let frame_beats = map.tempo_at_beat(at) / 60.0 / rate;
                        assert!(after < frame_beats * (1.0 + 1e-9), "{after} beats late");
                        assert!(
                            after > -1e-4 * frame_beats * (1.0 + 1e-9),
                            "{after} beats early"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn time_signatures_move_the_bars_after_them() {
    let mut map = song();
    assert_eq!(map.beat_at_bar(4), 16.0);
    assert_eq!(map.beat_at_bar(5), 19.5);
    assert_eq!(map.beat_at_bar(6), 23.0);
    assert_eq!(map.beat_at_bar(7), 26.0);
    assert_eq!(map.bar_at_beat(19.4).bar, 4);
    assert_eq!(map.bar_at_beat(19.5).bar, 5);
    assert_eq!(map.bar_at_beat(25.0).signature, TimeSignature::new(3, 4));
    let points: Vec<_> = map.signature_points().map(|p| (p.bar, p.beat)).collect();
    assert_eq!(points, [(0, 0.0), (4, 16.0), (6, 23.0)]);

    // 7/8 counts eighth notes, and bar 4 starts on beat 16, so beat 19
    // is its seventh eighth (6, counting from 0).
    let bbt = map.bbt_at_beat(19.0, 4);
    assert_eq!((bbt.bar, bbt.beat, bbt.tick), (4, 6, 0));
    assert_eq!(map.bbt_at_beat(19.25, 4).tick, 2);

    // A longer bar 4 pushes bar 6 later.
    map.set_signature(4, TimeSignature::new(9, 8)).unwrap();
    assert_eq!(map.beat_at_bar(6), 25.0);
    assert!(map.remove_signature(4));
    assert!(!map.remove_signature(4));
    assert!(!map.remove_signature(0));
    assert_eq!(map.beat_at_bar(6), 24.0);
}

#[test]
fn before_the_start_the_first_tempo_and_signature_hold() {
    let mut map = TempoMap::new(120.0, TimeSignature::new(3, 4)).unwrap();
    // Even when the first point ramps.
    map.set_tempo(0.0, 120.0, TempoCurve::Linear).unwrap();
    map.set_tempo(3.0, 60.0, TempoCurve::Step).unwrap();
    assert_eq!(map.seconds_at_beat(-3.0), -1.5);
    assert_eq!(map.beat_at_seconds(-1.5), -3.0);
    assert_eq!(map.tempo_at_beat(-1.0), 120.0);
    assert_eq!(map.bar_at_beat(-0.5).bar, -1);
    assert_eq!(map.bar_at_beat(-0.5).start_beat, -3.0);
    assert_eq!(map.beat_at_bar(-2), -6.0);
    let bbt = map.bbt_at_beat(-2.0, 10);
    assert_eq!((bbt.bar, bbt.beat, bbt.tick), (-1, 1, 0));
    assert_eq!(bbt.to_string(), "0.2.0");
    for rate in RATES {
        assert_eq!(map.frame_at_beat(-1.0, rate), -(rate as i64) / 2);
        assert_eq!(map.beat_at_frame(-(rate as i64), rate), -2.0);
        let position = map.position_at_frame(-(rate as i64), rate);
        assert_eq!(position.bar, -1);
        assert_eq!(position.tempo_bpm, 120.0);
    }
}

#[test]
fn edits_are_checked() {
    let mut map = TempoMap::default();
    assert_eq!(TempoMap::new(0.0, four_four()), Err(TempoMapError::Tempo));
    assert_eq!(
        TempoMap::new(120.0, TimeSignature::new(0, 4)),
        Err(TempoMapError::Signature)
    );
    assert_eq!(
        map.set_tempo(f64::NAN, 100.0, TempoCurve::Step),
        Err(TempoMapError::Position)
    );
    assert_eq!(
        map.set_tempo(-1.0, 100.0, TempoCurve::Step),
        Err(TempoMapError::Position)
    );
    assert_eq!(
        map.set_tempo(1.0, f64::INFINITY, TempoCurve::Step),
        Err(TempoMapError::Tempo)
    );
    assert_eq!(
        map.set_signature(-1, four_four()),
        Err(TempoMapError::Position)
    );
    assert_eq!(
        map.set_signature(2, TimeSignature::new(3, 0)),
        Err(TempoMapError::Signature)
    );
    assert_eq!(map, TempoMap::default());
    assert!(!TempoMapError::Tempo.to_string().is_empty());

    // Replacing and removing points.
    map.set_tempo(0.0, 90.0, TempoCurve::Step).unwrap();
    map.set_tempo(4.0, 60.0, TempoCurve::Step).unwrap();
    map.set_tempo(4.0, 180.0, TempoCurve::Step).unwrap();
    let points: Vec<_> = map.tempo_points().map(|p| (p.beat, p.bpm)).collect();
    assert_eq!(points, [(0.0, 90.0), (4.0, 180.0)]);
    assert!(!map.remove_tempo(0.0));
    assert!(!map.remove_tempo(3.0));
    assert!(map.remove_tempo(4.0));
    assert!(map.is_constant());
    assert_eq!(map.seconds_at_beat(3.0), 2.0);
}

#[test]
fn ticks_and_signatures_convert() {
    assert_eq!(beats_to_ticks(2.5, 960), 2_400.0);
    assert_eq!(ticks_to_beats(2_400, 960), 2.5);
    assert_eq!(ticks_to_beats(5, 0), 5.0);
    let signature = TimeSignature::new(6, 8);
    assert_eq!(signature.beat_length(), 0.5);
    assert_eq!(signature.bar_length(), 3.0);
    assert_eq!(signature.to_string(), "6/8");
    assert_eq!(<(u16, u16)>::from(signature), (6, 8));
    assert_eq!(TimeSignature::from((6, 8)), signature);
    assert_eq!(TimeSignature::default(), TimeSignature::FOUR_FOUR);
}

#[test]
fn a_midi_file_gives_the_same_times_as_its_reader() {
    let mut conductor = Track::new();
    conductor.push_meta(0, MetaEvent::Tempo(500_000));
    conductor.push_meta(
        0,
        MetaEvent::TimeSignature {
            numerator: 3,
            denominator_power: 2,
            clocks_per_click: 24,
            thirty_seconds_per_quarter: 8,
        },
    );
    conductor.push_meta(960, MetaEvent::Tempo(750_000));
    conductor.push_meta(2_000, MetaEvent::Tempo(400_000));
    // Not on a bar line: bar 2 starts at tick 2 880 (two bars of 3/4 at
    // 480 a quarter), so it takes effect at bar 3.
    conductor.push_meta(
        3_000,
        MetaEvent::TimeSignature {
            numerator: 7,
            denominator_power: 3,
            clocks_per_click: 12,
            thirty_seconds_per_quarter: 8,
        },
    );
    let mut notes = Track::new();
    notes.push_midi(
        4_800,
        MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );
    let file = MidiFile::multi_track(480, vec![conductor, notes]);
    let map = TempoMap::from_midi_file(&file).unwrap();
    for tick in (0..6_000).step_by(37) {
        let seconds = file.tick_to_seconds(tick).unwrap();
        close(map.seconds_at_beat(tick as f64 / 480.0), seconds, 1e-12);
    }
    assert_eq!(map.signature_at_bar(0), TimeSignature::new(3, 4));
    assert_eq!(map.signature_at_bar(2), TimeSignature::new(3, 4));
    assert_eq!(map.signature_at_bar(3), TimeSignature::new(7, 8));
    assert_eq!(map.beat_at_bar(3), 9.0);

    let mut smpte = file.clone();
    smpte.division = manguebyte_midi::file::Division::Smpte {
        frames_per_second: 25,
        ticks_per_frame: 40,
    };
    assert!(TempoMap::from_midi_file(&smpte).is_none());
}

// --- The clock --------------------------------------------------------------

fn offsets(block: &ClockBlock) -> Vec<(usize, usize, i64)> {
    block
        .pieces()
        .map(|piece| (piece.offset, piece.frames, piece.start_frame))
        .collect()
}

#[test]
fn a_stopped_clock_stands_still() {
    let mut clock = TransportClock::new(TempoMap::default(), 48_000.0);
    assert!(!clock.is_playing());
    let block = clock.next_block(256);
    assert!(!block.playing);
    assert_eq!(block.pieces().count(), 0);
    assert_eq!(block.frame_at(200), 0);
    assert_eq!(clock.position(), 0);
    let transport = block.transport(clock.map());
    assert!(!transport.playing);
    assert_eq!(transport.position_beats, Some(0.0));
    assert_eq!(transport.tempo_bpm, Some(120.0));
    assert_eq!(block.with_map(clock.map()).changes().count(), 0);

    clock.play();
    let block = clock.next_block(256);
    assert_eq!(offsets(&block), [(0, 256, 0)]);
    assert_eq!(clock.position(), 256);
    clock.stop();
    clock.next_block(256);
    assert_eq!(clock.position(), 256);
}

#[test]
fn a_loop_goes_round_inside_a_block() {
    let mut clock = TransportClock::new(TempoMap::default(), 48_000.0);
    clock.set_loop_frames(Some((100, 160)));
    clock.seek_frame(90);
    clock.play();

    let block = clock.next_block(200);
    assert!(block.jumped);
    // From before the loop, playback runs to its end, then goes round.
    assert_eq!(
        offsets(&block),
        [(0, 70, 90), (70, 60, 100), (130, 60, 100), (190, 10, 100)]
    );
    assert_eq!(block.wraps().collect::<Vec<_>>(), [70, 130, 190]);
    assert_eq!(block.frame_at(9), 99);
    assert_eq!(block.frame_at(10), 100);
    assert_eq!(block.frame_at(69), 159);
    assert_eq!(block.frame_at(70), 100);
    assert_eq!(block.frame_at(199), 109);
    assert_eq!(
        block.offsets_of_frame(105).collect::<Vec<_>>(),
        [15, 75, 135, 195]
    );
    assert_eq!(block.offsets_of_frame(95).collect::<Vec<_>>(), [5]);
    assert_eq!(clock.position(), 110);

    // Ending on the loop's end: the next block starts wrapped.
    let block = clock.next_block(50);
    assert!(!block.jumped && !block.wrapped);
    assert_eq!(offsets(&block), [(0, 50, 110)]);
    assert_eq!(clock.position(), 100);
    let block = clock.next_block(10);
    assert!(block.wrapped);
    assert_eq!(block.wraps().count(), 0);
    assert_eq!(block.start_frame, 100);

    // Past the loop's end, playback runs on.
    clock.seek_frame(500);
    let block = clock.next_block(100);
    assert_eq!(offsets(&block), [(0, 100, 500)]);
    // An empty loop is none.
    clock.set_loop_frames(Some((10, 10)));
    assert_eq!(clock.loop_frames(), None);
    clock.set_loop_frames(Some((i64::MIN, i64::MAX)));
    assert_eq!(clock.loop_frames(), None);
}

#[test]
fn a_loop_in_beats_reports_its_beats_and_follows_the_map() {
    let mut clock = TransportClock::new(TempoMap::default(), 48_000.0);
    clock.set_loop_beats(Some((4.0, 8.0)));
    assert_eq!(clock.loop_frames(), Some((96_000, 192_000)));
    assert_eq!(clock.transport().loop_beats, Some((4.0, 8.0)));

    // At 60 bpm the same beats are twice as many frames.
    let old = clock.replace_map(Box::new(TempoMap::new(60.0, four_four()).unwrap()));
    assert_eq!(*old, TempoMap::default());
    assert_eq!(clock.loop_frames(), Some((192_000, 384_000)));

    // A loop in frames reports the beats they fall on.
    clock.set_loop_frames(Some((48_000, 96_000)));
    assert_eq!(clock.transport().loop_beats, Some((1.0, 2.0)));
    clock.set_loop_beats(None);
    assert_eq!(clock.transport().loop_beats, None);
    clock.set_loop_beats(Some((f64::NAN, 2.0)));
    assert_eq!(clock.loop_frames(), None);
}

#[test]
fn seeking_jumps_and_a_new_map_keeps_the_beat() {
    let mut clock = TransportClock::new(TempoMap::default(), 48_000.0);
    clock.play();
    clock.seek_beat(3.0);
    let block = clock.next_block(64);
    assert!(block.jumped);
    assert_eq!(block.start_frame, 72_000);
    assert_eq!(block.transport(clock.map()).position_beats, Some(3.0));
    assert!(!clock.next_block(64).jumped);

    // Seeking where it already is is no jump.
    clock.seek_frame(clock.position());
    assert!(!clock.next_block(64).jumped);
    clock.seek_beat(f64::INFINITY);
    assert!(!clock.next_block(64).jumped);

    // Beat 4 at 120 bpm is frame 96 000; at 240 bpm it is 48 000.
    clock.seek_beat(4.0);
    clock.next_block(0);
    let _ = clock.replace_map(Box::new(TempoMap::new(240.0, four_four()).unwrap()));
    assert_eq!(clock.position(), 48_000);
    let block = clock.next_block(64);
    assert!(block.jumped);
    assert_eq!(block.transport(clock.map()).position_beats, Some(4.0));

    // So does a new sample rate.
    clock.set_sample_rate(96_000.0);
    assert_eq!(clock.position(), 96_000 + 128);
    assert_eq!(clock.sample_rate(), 96_000.0);
    clock.set_sample_rate(-1.0);
    assert_eq!(clock.sample_rate(), 96_000.0);
}

#[test]
fn a_ramp_that_crosses_a_block_is_placed_on_its_frames() {
    let mut map = TempoMap::new(120.0, four_four()).unwrap();
    map.set_tempo(1.0, 120.0, TempoCurve::Linear).unwrap();
    map.set_tempo(2.0, 180.0, TempoCurve::Step).unwrap();
    map.set_signature(1, TimeSignature::new(5, 8)).unwrap();
    for rate in RATES {
        let mut clock = TransportClock::new(map.clone(), rate);
        clock.play();
        // Beat 1 is half a second in; the ramp lasts 120 / 300 s.
        let start = map.frame_at_beat(1.0, rate);
        assert_eq!(start, (rate / 2.0) as i64);
        let end = map.frame_at_beat(2.0, rate);
        clock.seek_frame(start - 100);
        let length = (end - start + 200) as usize;
        let block = clock.next_block(length);
        let timeline = block.with_map(clock.map());

        let changes: Vec<_> = timeline.changes().collect();
        assert_eq!(changes.len(), 2, "{changes:?}");
        assert_eq!(changes[0].offset, 100);
        assert!(matches!(
            changes[0].kind,
            ChangeKind::Tempo(point) if point.bpm == 120.0 && point.curve == TempoCurve::Linear
        ));
        assert_eq!(changes[1].offset, (end - start + 100) as usize);
        assert_eq!(changes[1].frame, end);
        assert!(matches!(changes[1].kind, ChangeKind::Tempo(point) if point.bpm == 180.0));

        // The transport at the block's start knows nothing of the ramp
        // yet; the timeline gives every frame its tempo.
        let transport = timeline.transport();
        assert_eq!(transport.tempo_bpm, Some(120.0));
        assert_eq!(transport.tempo_increment, Some(0.0));
        let mut last = 0.0;
        for offset in (0..length).step_by(97) {
            let tempo = timeline.tempo_at(offset);
            assert!(tempo >= last);
            last = tempo;
            assert_eq!(tempo, map.tempo_at_frame(start - 100 + offset as i64, rate));
        }
        close(timeline.tempo_at(length - 1), 180.0, 0.0);
        assert_eq!(
            timeline.offset_of_beat(1.5),
            Some((map.frame_at_beat(1.5, rate) - start + 100) as usize)
        );
        assert_eq!(timeline.offset_of_beat(100.0), None);
        // A block starting inside the ramp is told its slope.
        clock.seek_frame(start + 10);
        let block = clock.next_block(16);
        let inside = block.transport(clock.map());
        close(inside.tempo_increment.unwrap(), 60.0 / 0.4 / rate, 1e-15);
        assert_eq!(inside.bar_number, Some(0));
        assert_eq!(inside.time_signature, Some((4, 4)));
    }
}

#[test]
fn changes_on_one_frame_come_signature_first_and_follow_loops() {
    let mut map = TempoMap::new(120.0, four_four()).unwrap();
    map.set_tempo(4.0, 60.0, TempoCurve::Step).unwrap();
    map.set_signature(1, TimeSignature::new(3, 4)).unwrap();
    let mut clock = TransportClock::new(map, 1_000.0);
    // Beat 4 is frame 2 000; loop over [1 990, 2 010).
    clock.set_loop_frames(Some((1_990, 2_010)));
    clock.seek_frame(1_995);
    clock.play();
    let block = clock.next_block(40);
    let changes: Vec<_> = block
        .with_map(clock.map())
        .changes()
        .map(|change| {
            (
                change.offset,
                matches!(change.kind, ChangeKind::Signature(_)),
            )
        })
        .collect();
    // Offsets 5 and 25: the frame comes round once more in the block.
    assert_eq!(changes, [(5, true), (5, false), (25, true), (25, false)]);
}

/// Everything a block pattern sees, frame by frame, on the render
/// timeline.
fn trace(pattern: &[usize], total: usize, rate: f64) -> Vec<(i64, u64, u64, i64, u32)> {
    let mut clock = TransportClock::new(song(), rate);
    // Round the end of the first ramp and the start of the 7/8 bars.
    clock.set_loop_beats(Some((15.0, 16.5)));
    clock.seek_beat(14.5);
    clock.play();
    let mut seen = Vec::with_capacity(total);
    let mut changes = Vec::new();
    let mut done = 0;
    let mut next = 0;
    while done < total {
        let length = pattern[next % pattern.len()].min(total - done);
        next += 1;
        let block = clock.next_block(length);
        let timeline = block.with_map(clock.map());
        let transport = timeline.transport();
        let first = timeline.position_at(0);
        assert_eq!(transport.position_beats, Some(first.beat));
        assert_eq!(transport.bar_number, Some(first.bar));
        for offset in 0..length {
            let position = timeline.position_at(offset);
            seen.push((
                position.frame,
                position.beat.to_bits(),
                position.tempo_bpm.to_bits(),
                position.bar,
                0,
            ));
        }
        for change in timeline.changes() {
            changes.push(done + change.offset);
        }
        done += length;
    }
    for at in changes {
        seen[at].4 += 1;
    }
    seen
}

#[test]
fn positions_do_not_depend_on_the_block_pattern() {
    for rate in RATES {
        // A second and a half: the loop goes round twice.
        let total = if cfg!(miri) {
            3_000
        } else {
            (1.5 * rate) as usize
        };
        let reference = trace(&[total], total, rate);
        if !cfg!(miri) {
            // The loop went round, and the changes in it were found on
            // each lap.
            assert!(reference.windows(2).any(|pair| pair[1].0 < pair[0].0));
            // Beat 16 has a tempo point and a time signature.
            let changes: u32 = reference.iter().map(|seen| seen.4).sum();
            assert!(changes >= 4, "{changes} changes");
        }
        for pattern in [
            &[1usize][..],
            &[64],
            &[512],
            &[7, 300, 1, 4_096],
            &[1_000, 3],
        ] {
            if cfg!(miri) && pattern == [1] {
                continue;
            }
            assert!(
                trace(pattern, total, rate) == reference,
                "pattern {pattern:?} at {rate} differs"
            );
        }
    }
}

// --- The control --------------------------------------------------------------

#[test]
fn a_control_works_the_clock_from_another_thread() {
    let (mut clock, control) = TransportClock::with_control(TempoMap::default(), 48_000.0);
    assert!(!control.is_playing());
    assert!(control.play());
    assert!(control.set_recording(true));
    assert!(control.seek_beat(2.0));
    assert!(control.set_loop_beats(Some((2.0, 4.0))));
    // Nothing happens before the next block.
    assert!(!clock.is_playing());

    let worker = std::thread::spawn(move || {
        let block = clock.next_block(48_000);
        (clock, block)
    });
    let (mut clock, block) = worker.join().unwrap();
    assert!(block.playing && block.recording && block.jumped);
    assert_eq!(block.start_frame, 48_000);
    assert_eq!(block.transport(clock.map()).loop_beats, Some((2.0, 4.0)));
    // A whole lap: it ends on the loop's end and starts again.
    assert_eq!(clock.position(), 48_000);
    assert!(control.is_playing());
    assert!(control.is_recording());
    assert_eq!(control.position_frames(), 48_000);
    assert_eq!(control.position_beats(), 2.0);
    assert_eq!(control.tempo_bpm(), 120.0);

    // A new map keeps the beat; the old one comes back to be freed here.
    let faster = TempoMap::new(240.0, four_four()).unwrap();
    control.set_tempo_map(faster.clone()).unwrap();
    clock.next_block(0);
    assert_eq!(clock.map(), &faster);
    assert_eq!(clock.position(), 24_000);
    assert_eq!(control.tempo_bpm(), 240.0);
    assert_eq!(control.collect(), 1);
    assert_eq!(control.collect(), 0);

    // No more than four maps wait at once.
    for _ in 0..4 {
        control.set_tempo_map(TempoMap::default()).unwrap();
    }
    assert!(control.set_tempo_map(TempoMap::default()).is_err());
    clock.next_block(0);
    assert!(control.set_tempo_map(TempoMap::default()).is_ok());
    assert_eq!(control.collect(), 0);

    assert!(control.stop());
    assert!(control.set_loop_frames(None));
    assert!(control.seek_frame(0));
    clock.next_block(10);
    assert!(!control.is_playing());
    assert_eq!(clock.loop_frames(), None);
    assert!(format!("{control:?}").contains("ClockControl"));
    assert!(format!("{clock:?}").contains("TransportClock"));
}

#[test]
fn a_full_command_queue_refuses_more() {
    let (mut clock, control) = TransportClock::with_control(TempoMap::default(), 48_000.0);
    let mut sent = 0;
    while control.seek_frame(sent) {
        sent += 1;
    }
    assert!(sent >= 64);
    clock.next_block(1);
    assert_eq!(clock.position(), sent - 1);
    assert!(control.play());
}
