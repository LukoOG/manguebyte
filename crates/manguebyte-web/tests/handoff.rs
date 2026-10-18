//! Control calls in two halves (`Engine::attach_part` and the others):
//! made off the audio thread, handed over on it without touching the heap,
//! and heard exactly as the control calls they stand for.
//!
//! One scenario is played twice: by an engine given the plain control
//! calls between its quanta, and by one given the same calls made into
//! `Detached*` slots, with a counting allocator watching every hand-over,
//! every render and every instrument handed back. The two outputs must be
//! the same, bit for bit, with calls made while notes sound.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::*;
use manguebyte_web::{
    Chain, DetachedInstrument, DetachedPart, DetachedSfz, DetachedStages, Effect, Engine,
    EngineConfig, EngineError, EventRecord, HandoffError, InstrumentKind, SfzInstrument,
};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn record() {
        // `try_with` because the allocator is still called while a thread's
        // locals are being torn down.
        let _ = WATCHING.try_with(|watching| {
            if watching.get() {
                let _ = HEAP_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            }
        });
    }
}

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is bumping a thread-local counter, which does not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `work` and returns how many heap calls it made on this thread.
fn heap_calls<R>(work: impl FnOnce() -> R) -> (R, usize) {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    let result = work();
    WATCHING.with(|watching| watching.set(false));
    (result, HEAP_CALLS.with(Cell::get))
}

/// A control call of the scenario.
#[derive(Clone, Copy)]
enum Call {
    Create(u16, InstrumentKind, Chain),
    Instrument(u16, InstrumentKind),
    SetChain(u16, Chain),
    Remove(u16),
    /// Load instrument 0 (the tone) or 1 (the kit) into a sampler part.
    Sfz(u16, usize),
}

/// An SFZ text and the files it names.
type Sfz = (String, Vec<(String, Vec<u8>)>);

fn instruments() -> [Sfz; 2] {
    [tone_instrument(), kit_instrument()]
}

/// The calls, each on the frame it is made before, in order.
fn calls() -> Vec<(u64, Call)> {
    let delay = Chain::FULL.with(Effect::Delay);
    vec![
        (0, Call::Create(0, InstrumentKind::Synth, Chain::FULL)),
        (0, Call::Create(1, InstrumentKind::Drums, Chain::NONE)),
        (0, Call::Create(2, InstrumentKind::Sampler, Chain::FULL)),
        (0, Call::Sfz(2, 0)),
        (0, Call::Create(3, InstrumentKind::Plucked, delay)),
        (0, Call::Create(4, InstrumentKind::SamplerKit, Chain::NONE)),
        // While part 0 sounds: two effects it has never had.
        (
            12_000,
            Call::SetChain(0, Chain::FULL.with(Effect::Chorus).with(Effect::Drive)),
        ),
        (20_000, Call::Instrument(1, InstrumentKind::AnalogKit)),
        (24_000, Call::Sfz(4, 1)),
        // Made again over itself, while it sounds.
        (30_000, Call::Create(0, InstrumentKind::Fm, Chain::NONE)),
        (36_000, Call::SetChain(3, Chain::NONE)),
        (40_000, Call::Remove(3)),
        // A second instrument while the first one's note rings.
        (45_000, Call::Sfz(2, 0)),
        (50_000, Call::SetChain(1, Chain::NONE.with(Effect::Delay))),
    ]
}

fn records() -> Vec<EventRecord> {
    let mut records = Vec::new();
    for step in 0..24u64 {
        let at = 1_000 + step * 2_500;
        records.push(
            EventRecord::note_on(at, 0, 52 + (step % 7) as u8, 0.8).with_accent(step % 3 == 0),
        );
        records.push(EventRecord::note_off(at + 2_000, 0, 52 + (step % 7) as u8));
        records.push(EventRecord::note_on(
            at + 300,
            1,
            [36, 38, 42, 46][step as usize % 4],
            0.9,
        ));
        records.push(EventRecord::note_on(
            at + 600,
            2,
            57 + (step % 5) as u8,
            0.7,
        ));
        records.push(EventRecord::note_off(at + 2_400, 2, 57 + (step % 5) as u8));
        records.push(EventRecord::note_on(
            at + 900,
            3,
            40 + (step % 12) as u8,
            0.9,
        ));
        records.push(EventRecord::note_off(at + 1_900, 3, 40 + (step % 12) as u8));
        records.push(EventRecord::note_on(
            at + 1_200,
            4,
            [36, 38, 42, 49][step as usize % 4],
            1.0,
        ));
    }
    // The note part 2 holds across the second load.
    records.push(EventRecord::note_on(44_000, 2, 69, 0.8));
    records.push(EventRecord::note_off(52_000, 2, 69));
    records.push(EventRecord::param(5_000, 0, id("lowpass.cutoff"), 900.0));
    records.push(EventRecord::param(13_000, 0, id("chorus.on"), 1.0));
    records.push(EventRecord::param(13_000, 0, id("drive.amount"), 0.6));
    records.push(EventRecord::param(13_000, 0, id("drive.on"), 1.0));
    records.push(EventRecord::param(8_000, 3, id("delay.on"), 1.0));
    records.push(EventRecord::param(8_000, 3, id("delay.time"), 0.09));
    records.push(EventRecord::param(51_000, 1, id("delay.on"), 1.0));
    records.push(EventRecord::param(21_000, 0, id("master.tempo"), 97.0));
    records
}

fn frames() -> usize {
    if cfg!(miri) { 2_000 } else { 60_000 }
}

fn config() -> EngineConfig {
    EngineConfig {
        pending_capacity: 1 << 12,
        ..EngineConfig::new(RATE)
    }
}

/// The scenario through the plain control calls, in quanta of 128.
fn plain() -> (Vec<f32>, Vec<f32>, Option<f64>) {
    let sfz = instruments();
    let mut engine = Engine::new(config()).unwrap();
    push_all(&mut engine, &records());
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut pending = calls().into_iter().peekable();
    while left.len() < frames() {
        let now = left.len() as u64;
        while let Some((_, call)) = pending.next_if(|&(at, _)| at <= now) {
            match call {
                Call::Create(part, kind, chain) => {
                    engine.create_part_with(part, kind, chain).unwrap()
                }
                Call::Instrument(part, kind) => engine.set_instrument(part, kind).unwrap(),
                Call::SetChain(part, chain) => engine.set_chain(part, chain).unwrap(),
                Call::Remove(part) => engine.remove_part(part).unwrap(),
                Call::Sfz(part, which) => {
                    let (text, files) = &sfz[which];
                    engine
                        .load_instrument(part, build_sfz(text, files))
                        .unwrap();
                }
            }
        }
        let next = pending.peek().map_or(u64::MAX, |&(at, _)| at);
        let quantum = 128.min(frames() - left.len()).min((next - now) as usize);
        engine.render(quantum);
        let (l, r) = engine.output();
        left.extend_from_slice(l);
        right.extend_from_slice(r);
    }
    (left, right, engine.sampler_make_up_db(2))
}

/// What a call becomes off the audio thread.
enum Slot {
    Part(DetachedPart),
    Instrument(u16, DetachedInstrument),
    Stages(u16, Chain, DetachedStages),
    Remove(u16, DetachedPart),
    Sfz(u16, DetachedSfz),
}

fn detach(call: Call, sfz: &[Sfz]) -> Slot {
    let config = config();
    match call {
        Call::Create(part, kind, chain) => {
            Slot::Part(DetachedPart::new(&config, part, kind, chain).unwrap())
        }
        Call::Instrument(part, kind) => {
            Slot::Instrument(part, DetachedInstrument::new(&config, kind).unwrap())
        }
        Call::SetChain(part, chain) => {
            Slot::Stages(part, chain, DetachedStages::new(&config, chain).unwrap())
        }
        Call::Remove(part) => Slot::Remove(part, DetachedPart::empty()),
        Call::Sfz(part, which) => {
            let (text, files) = &sfz[which];
            // Instrument 1 is the kit, for the sampler kit part.
            let kit = which == 1;
            Slot::Sfz(
                part,
                DetachedSfz::measured(&config, build_sfz(text, files), kit).unwrap(),
            )
        }
    }
}

/// Hands `slot` over; never allocates or frees.
fn hand_over(engine: &mut Engine, slot: &mut Slot) -> Result<(), HandoffError> {
    match slot {
        Slot::Part(part) => engine.attach_part(part),
        Slot::Instrument(part, instrument) => engine.swap_instrument(*part, instrument),
        Slot::Stages(part, chain, stages) => engine.set_chain_from(*part, *chain, stages),
        Slot::Remove(part, into) => engine.detach_part(*part, into),
        Slot::Sfz(part, sfz) => engine.give_sfz(*part, sfz),
    }
}

/// The scenario through the hand-overs, in blocks that keep changing size.
fn handed_over() -> (Vec<f32>, Vec<f32>, Option<f64>, usize) {
    let sfz = instruments();
    let mut engine = Engine::new(config()).unwrap();
    push_all(&mut engine, &records());
    let mut left = vec![0.0; frames()];
    let mut right = vec![0.0; frames()];
    let pattern = [1usize, 77, 512, 300, 129];
    let mut next_block = 0;
    let mut done = 0usize;
    let mut finished: Vec<Box<SfzInstrument>> = Vec::with_capacity(16);
    let mut pending = calls().into_iter().peekable();
    let mut heap = 0;
    while done < frames() {
        let now = done as u64;
        // Off the audio thread: what the calls due now make.
        let mut slots: Vec<Slot> = Vec::new();
        while let Some((_, call)) = pending.next_if(|&(at, _)| at <= now) {
            slots.push(detach(call, &sfz));
        }
        let next = pending.peek().map_or(u64::MAX, |&(at, _)| at);
        let block = pattern[next_block % pattern.len()]
            .min(frames() - done)
            .min((next - now) as usize);
        next_block += 1;
        // On it: the hand-overs, the block, and what comes back.
        let ((), calls) = heap_calls(|| {
            for slot in &mut slots {
                hand_over(&mut engine, slot).unwrap();
            }
            engine.render_into(
                &mut left[done..done + block],
                &mut right[done..done + block],
            );
            while finished.len() < finished.capacity() {
                match engine.take_finished_instrument() {
                    Some(instrument) => finished.push(instrument),
                    None => break,
                }
            }
        });
        heap += calls;
        // Off it again: what the engine let go of is dropped here.
        drop(slots);
        done += block;
    }
    let make_up = engine.sampler_make_up_db(2);
    assert!(
        cfg!(miri) || !finished.is_empty(),
        "the first instrument of part 2 came back"
    );
    (left, right, make_up, heap)
}

#[test]
fn the_counter_counts() {
    assert!(heap_calls(|| drop(vec![1u8; 16])).1 >= 2);
}

#[test]
fn handing_over_sounds_like_the_control_calls_and_never_touches_the_heap() {
    let (left, right, make_up) = plain();
    let (hand_left, hand_right, hand_make_up, heap) = handed_over();
    assert_eq!(
        heap, 0,
        "the hand-overs, renders or hand-backs touched the heap"
    );
    assert!(peak(&left) > 0.05, "the scenario is heard");
    assert_eq!(make_up, hand_make_up, "the same make-up was measured");
    assert!(cfg!(miri) || make_up.is_some());
    for (channel, (a, b)) in [(&left, &hand_left), (&right, &hand_right)]
        .into_iter()
        .enumerate()
    {
        if let Some(frame) = a
            .iter()
            .zip(b.iter())
            .position(|(x, y)| x.to_bits() != y.to_bits())
        {
            panic!(
                "channel {channel} differs from frame {frame}: {} against {}",
                a[frame], b[frame]
            );
        }
    }
}

#[test]
fn refusals_leave_the_engine_and_the_slot_as_they_were() {
    let mut engine = Engine::new(EngineConfig {
        max_parts: 1,
        ..config()
    })
    .unwrap();
    let mut first = DetachedPart::new(&config(), 0, InstrumentKind::Synth, Chain::FULL).unwrap();
    engine.attach_part(&mut first).unwrap();
    assert!(first.is_empty(), "a new part leaves nothing behind");
    assert_eq!(engine.attach_part(&mut first), Err(HandoffError::Empty));

    // Every slot is taken: the part stays in its slot.
    let mut second = DetachedPart::new(&config(), 1, InstrumentKind::Drums, Chain::NONE).unwrap();
    assert_eq!(
        engine.attach_part(&mut second),
        Err(HandoffError::Engine(EngineError::TooManyParts { max: 1 }))
    );
    assert_eq!(second.number(), Some(1));

    // Made at another rate.
    let other = EngineConfig::new(44_100.0);
    let mut foreign = DetachedPart::new(&other, 0, InstrumentKind::Synth, Chain::FULL).unwrap();
    assert_eq!(
        engine.attach_part(&mut foreign),
        Err(HandoffError::Mismatch)
    );
    assert_eq!(
        engine.swap_instrument(
            0,
            &mut DetachedInstrument::new(&other, InstrumentKind::Fm).unwrap()
        ),
        Err(HandoffError::Mismatch)
    );

    // Replacing gives the old part back, and its kind says so.
    let mut again = DetachedPart::new(&config(), 0, InstrumentKind::Organ, Chain::NONE).unwrap();
    engine.attach_part(&mut again).unwrap();
    assert_eq!(again.kind(), Some(InstrumentKind::Synth));
    assert_eq!(engine.part_kind(0), Some(InstrumentKind::Organ));

    // Stages that lack an effect the part has never had.
    let mut stages = DetachedStages::new(&config(), Chain::NONE.with(Effect::Delay)).unwrap();
    assert_eq!(
        engine.set_chain_from(0, Chain::NONE.with(Effect::Chorus), &mut stages),
        Err(HandoffError::Mismatch)
    );
    assert_eq!(engine.part_chain(0), Some(Chain::NONE));
    engine
        .set_chain_from(0, Chain::NONE.with(Effect::Delay), &mut stages)
        .unwrap();
    assert_eq!(engine.part_chain(0), Some(Chain::NONE.with(Effect::Delay)));

    // An instrument for the wrong part, the wrong kind of sampler, or none.
    let (text, files) = tone_instrument();
    let mut sfz = DetachedSfz::measured(&config(), build_sfz(&text, &files), false).unwrap();
    assert_eq!(
        engine.give_sfz(0, &mut sfz),
        Err(HandoffError::Engine(EngineError::NotASampler(0)))
    );
    let mut kit = DetachedPart::new(&config(), 0, InstrumentKind::SamplerKit, Chain::NONE).unwrap();
    engine.attach_part(&mut kit).unwrap();
    assert_eq!(engine.give_sfz(0, &mut sfz), Err(HandoffError::Mismatch));
    assert!(!sfz.is_empty());

    // Taking a part out needs an empty slot, and a part.
    let mut into = DetachedPart::empty();
    assert_eq!(
        engine.detach_part(7, &mut into),
        Err(HandoffError::Engine(EngineError::NoSuchPart(7)))
    );
    engine.detach_part(0, &mut into).unwrap();
    assert_eq!(into.kind(), Some(InstrumentKind::SamplerKit));
    assert_eq!(
        engine.detach_part(0, &mut into),
        Err(HandoffError::Occupied)
    );
    assert_eq!(engine.part_ids().count(), 0);
}
