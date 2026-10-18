//! The whole driver path, from `init` to the host callbacks, against the
//! fake driver: no hardware, so these run on every system.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use manguebyte_core::AudioBuffer;
use manguebyte_devices::Direction;
use manguebyte_pro_io::sys;
use manguebyte_pro_io::{
    BlockInfo, BufferSizes, Error, FakeDevice, Notifications, SampleType, StreamConfig,
    loaded_driver,
};

/// A process loads one driver at a time, so the tests take turns.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Waits up to a second for `condition`, which the driver thread makes
/// true some milliseconds after a notification.
fn eventually(mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    condition()
}

fn duplex() -> StreamConfig {
    StreamConfig::new().with_inputs(0..2).with_outputs(0..2)
}

fn through(input: &mut AudioBuffer<f32>, output: &mut AudioBuffer<f32>, _: &BlockInfo) {
    for channel in 0..output.num_channels().min(input.num_channels()) {
        output
            .channel_mut(channel)
            .copy_from_slice(input.channel(channel));
    }
}

#[test]
fn a_process_loads_one_driver_and_releases_it_when_dropped() {
    let _turn = one_at_a_time();
    let first = FakeDevice::new("First");
    let second = FakeDevice::new("Second");
    let driver = first.load().unwrap();
    assert_eq!(driver.name(), "First");
    assert_eq!(loaded_driver().as_deref(), Some("First"));
    assert_eq!(
        second.load().unwrap_err(),
        Error::AlreadyLoaded {
            loaded: String::from("First")
        }
    );
    assert_eq!((first.instances_alive(), first.inits()), (1, 1));
    drop(driver);
    assert_eq!(first.instances_alive(), 0);
    assert_eq!(loaded_driver(), None);
    let driver = second.load().unwrap();
    assert_eq!(driver.key(), "Second");
}

#[test]
fn a_driver_that_will_not_initialise_says_why_and_is_released() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Unplugged").with_failing_init(true);
    match device.load() {
        Err(Error::InitFailed { driver, message }) => {
            assert_eq!(driver, "Unplugged");
            assert_eq!(message, "the fake device was told to fail");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(device.instances_alive(), 0);
    assert_eq!(loaded_driver(), None);
}

#[test]
fn the_driver_answers_every_query() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Fake").with_sample_types(
        vec![
            SampleType::Int24Lsb,
            SampleType::Int32Lsb,
            SampleType::Int16Lsb,
        ],
        vec![SampleType::Float32Lsb],
    );
    let driver = device.load().unwrap();
    let info = driver.info().unwrap();
    assert_eq!((info.channels.inputs, info.channels.outputs), (3, 1));
    assert_eq!(
        info.buffer_sizes,
        BufferSizes {
            min: 64,
            max: 2_048,
            preferred: 256,
            granularity: -1
        }
    );
    assert_eq!((info.latencies.input, info.latencies.output), (271, 287));
    assert_eq!(info.sample_rate, 48_000.0);
    assert_eq!(info.sample_rates, [44_100.0, 48_000.0, 96_000.0]);
    assert_eq!(info.inputs[1].name, "Fake In 2");
    assert_eq!(info.inputs[1].sample_type, SampleType::Int32Lsb);
    assert_eq!(info.outputs[0].direction, Direction::Output);
    assert!(!info.outputs[0].is_active);
    assert_eq!(info.clock_sources.len(), 1);
    assert_eq!(info.clock_sources[0].name, "Internal");
    assert!(info.clock_sources[0].is_current);
    assert!(info.can_time_info && info.can_report_overload);
    assert_eq!(info.internal_buffering, Some((16, 32)));

    assert!(driver.can_sample_rate(96_000.0).unwrap());
    assert!(!driver.can_sample_rate(22_050.0).unwrap());
    driver.set_sample_rate(96_000.0).unwrap();
    assert_eq!(driver.sample_rate().unwrap(), 96_000.0);
    assert!(matches!(
        driver.set_sample_rate(1.0),
        Err(Error::Driver {
            call: "setSampleRate",
            code: sys::ASE_NO_CLOCK
        })
    ));
    driver.set_clock_source(0).unwrap();
    assert!(driver.set_clock_source(3).is_err());
    let sizes = driver.buffer_sizes().unwrap();
    assert_eq!(sizes.sizes(), [64, 128, 256, 512, 1_024, 2_048]);
    assert_eq!((sizes.nearest(100), sizes.nearest(9_999)), (128, 2_048));
    assert!(sizes.contains(512) && !sizes.contains(500));
}

#[test]
fn blocks_alternate_halves_and_carry_audio_and_time() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Fake");
    let driver = device.load().unwrap();
    let seen = Arc::new(Mutex::new(Vec::with_capacity(16)));
    let record = Arc::clone(&seen);
    let stream = driver
        .start(&duplex(), move |input, output, info| {
            through(input, output, info);
            if let Ok(mut seen) = record.try_lock() {
                if seen.len() < seen.capacity() {
                    seen.push(*info);
                }
            }
        })
        .unwrap();
    assert_eq!(
        (stream.buffer_size(), stream.sample_rate()),
        (256, 48_000.0)
    );
    assert_eq!(device.buffer_size(), Some(256));
    assert!(device.is_running());
    assert_eq!(device.host_engine_version(), Some(2));
    assert!(device.host_takes_time_info());

    for block in 0..4u32 {
        let samples: Vec<f32> = (0..256).map(|i| (i + block) as f32 / 1_024.0).collect();
        let mut bytes = vec![0u8; 256 * 4];
        SampleType::Float32Lsb.write(&samples, &mut bytes).unwrap();
        assert!(device.write_input(1, &bytes));
        assert!(device.switch());
        assert_eq!(device.read_output(1).unwrap(), bytes, "block {block}");
        assert_eq!(device.read_output(0).unwrap(), vec![0u8; 256 * 4]);
    }
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 4);
    for (block, info) in seen.iter().enumerate() {
        assert_eq!(info.buffer_index, block % 2);
        assert_eq!(info.frame, 256 * block as u64);
        assert_eq!(info.sample_position, Some(256 * block as u64));
        assert!(info.system_time.is_some());
        assert!(info.time_info && info.direct_process);
        assert_eq!((info.input_latency, info.output_latency), (271, 287));
        assert_eq!(info.sample_rate, 48_000.0);
    }
    assert!(stream.uses_time_info());
    assert_eq!((stream.callbacks(), stream.frames()), (4, 1_024));
    assert!(!stream.uses_output_ready());
    assert_eq!(device.output_ready_calls(), 0);
}

#[test]
fn without_time_info_the_stream_asks_where_the_driver_is() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Old").with_time_info(false);
    let driver = device.load().unwrap();
    assert!(!driver.info().unwrap().can_time_info);
    let positions = Arc::new(AtomicU64::new(u64::MAX));
    let seen = Arc::clone(&positions);
    let stream = driver
        .start(&duplex(), move |_, _, info| {
            assert!(!info.time_info);
            seen.store(info.sample_position.unwrap_or(u64::MAX), Ordering::Relaxed);
        })
        .unwrap();
    assert!(!device.host_takes_time_info());
    assert!(device.switch());
    assert_eq!(positions.load(Ordering::Relaxed), 0);
    assert!(device.switch());
    assert_eq!(positions.load(Ordering::Relaxed), 256);
    assert!(!stream.uses_time_info());
}

#[test]
fn output_ready_follows_every_block_when_the_driver_wants_it() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Ready").with_output_ready(true);
    let driver = device.load().unwrap();
    let stream = driver.start(&duplex(), through).unwrap();
    assert!(stream.uses_output_ready());
    for _ in 0..3 {
        assert!(device.switch());
    }
    assert_eq!(device.output_ready_calls(), 3);
}

/// Values every type holds exactly, so that what goes in comes out bit for
/// bit after the trip through `f32`.
fn exact_signal(sample_type: SampleType, frames: usize) -> Vec<f32> {
    let bits = sample_type.bits().unwrap().min(24);
    let step = 1.0 / f64::from(1u32 << (bits - 1));
    let mut signal: Vec<f32> = (1..frames)
        .map(|frame| {
            let level = (frame as f64 * 37.0 % 255.0 - 127.0) / 128.0;
            ((level / step).round() * step) as f32
        })
        .collect();
    signal.push(-1.0);
    signal
}

#[test]
fn every_sample_type_goes_through_a_stream_and_back() {
    let _turn = one_at_a_time();
    for sample_type in SampleType::ALL {
        let device = FakeDevice::new("Types")
            .with_channels(1, 1, sample_type)
            .with_buffer_sizes(BufferSizes {
                min: 64,
                max: 64,
                preferred: 64,
                granularity: 0,
            });
        let driver = device.load().unwrap();
        let config = StreamConfig::new().with_inputs([0]).with_outputs([0]);
        if sample_type.is_dsd() {
            assert_eq!(
                driver.start(&config, through).unwrap_err(),
                Error::UnsupportedSampleType {
                    direction: Direction::Input,
                    channel: 0,
                    sample_type
                }
            );
            continue;
        }
        let peak = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&peak);
        let stream = driver
            .start(&config, move |input, output, info| {
                seen.store(u64::from(input.peak().to_bits()), Ordering::Relaxed);
                through(input, output, info);
            })
            .unwrap();
        let signal = exact_signal(sample_type, 64);
        let mut bytes = vec![0u8; sample_type.buffer_bytes(64).unwrap()];
        sample_type.write(&signal, &mut bytes).unwrap();
        assert!(device.write_input(0, &bytes));
        assert!(device.switch());
        assert_eq!(device.read_output(0).unwrap(), bytes, "{sample_type}");
        let expected = signal.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert_eq!(
            f32::from_bits(peak.load(Ordering::Relaxed) as u32),
            expected,
            "{sample_type}"
        );
        drop(stream);
    }
}

#[test]
fn a_reset_request_is_heard_off_the_audio_thread_and_the_stream_goes_on() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Reset");
    let driver = device.load().unwrap();
    let (heard, listen) = mpsc::channel();
    let main = std::thread::current().id();
    let blocks = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&blocks);
    let stream = driver
        .start_with_listener(
            &duplex(),
            move |_, _, _| {
                counted.fetch_add(1, Ordering::Relaxed);
            },
            move |notifications: &Notifications| {
                assert_ne!(std::thread::current().id(), main);
                let _ = heard.send(*notifications);
            },
        )
        .unwrap();
    assert!(!stream.needs_restart());
    // The driver asks inside its own call: the host only counts it.
    assert_eq!(device.request_reset(), Some(1));
    let notifications = listen.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(notifications.reset_requests, 1);
    assert!(notifications.needs_restart());
    assert!(stream.needs_restart());
    assert!(device.switch());
    assert_eq!(blocks.load(Ordering::Relaxed), 1);
    // Reopening is the owner's job: drop both, load again.
    drop(stream);
    drop(driver);
    let driver = device.load().unwrap();
    let stream = driver.start(&duplex(), through).unwrap();
    assert!(!stream.needs_restart());
    assert_eq!(
        (device.instances_created(), device.instances_alive()),
        (2, 1)
    );
}

#[test]
fn buffer_size_latency_resync_and_overload_reports_are_counted() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Reports");
    let driver = device.load().unwrap();
    let latency = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&latency);
    let stream = driver
        .start(&duplex(), move |_, _, info| {
            seen.store(u64::from(info.output_latency), Ordering::Relaxed);
        })
        .unwrap();

    assert_eq!(device.report_overload(), Some(0));
    assert!(eventually(|| stream.notifications().overloads == 1));
    assert!(!stream.needs_restart(), "an overload needs no restart");

    assert_eq!(device.change_latencies(100, 200), Some(1));
    assert!(eventually(|| stream.latencies().output == 200));
    assert_eq!(stream.latencies().input, 100);
    assert!(device.switch());
    assert_eq!(latency.load(Ordering::Relaxed), 200);
    assert!(!stream.needs_restart());

    assert_eq!(device.change_buffer_size(512), Some(1));
    let notifications = stream.notifications();
    assert_eq!(
        (
            notifications.buffer_size_changes,
            notifications.new_buffer_size
        ),
        (1, Some(512))
    );
    assert!(stream.needs_restart());
    assert_eq!(device.request_resync(), Some(1));
    assert_eq!(stream.notifications().resync_requests, 1);
}

#[test]
fn a_sample_rate_change_is_reported_both_ways() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Clock");
    let driver = device.load().unwrap();
    let stream = driver.start(&duplex(), through).unwrap();
    assert!(device.change_sample_rate(96_000.0));
    let notifications = stream.notifications();
    assert_eq!(notifications.sample_rate_changes, 1);
    assert_eq!(notifications.new_sample_rate, Some(96_000.0));
    assert!(stream.needs_restart());
    // The next block's time info says so too.
    assert!(device.switch());
    assert_eq!(stream.notifications().sample_rate_changes, 2);
    // A lost clock is a rate of zero.
    assert!(device.change_sample_rate(0.0));
    assert_eq!(stream.notifications().new_sample_rate, Some(0.0));
}

#[test]
fn the_host_answers_the_selectors_the_sdk_documents() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Questions");
    assert_eq!(device.ask_host(sys::ENGINE_VERSION, 0), None);
    let driver = device.load().unwrap();
    let _stream = driver.start(&duplex(), through).unwrap();
    for (selector, supported) in [
        (sys::ENGINE_VERSION, 1),
        (sys::RESET_REQUEST, 1),
        (sys::BUFFER_SIZE_CHANGE, 1),
        (sys::RESYNC_REQUEST, 1),
        (sys::LATENCIES_CHANGED, 1),
        (sys::SUPPORTS_TIME_INFO, 1),
        (sys::OVERLOAD, 1),
        (sys::SUPPORTS_TIME_CODE, 0),
        (sys::MMC_COMMAND, 0),
        (sys::SUPPORTS_INPUT_MONITOR, 0),
        (sys::SUPPORTS_INPUT_GAIN, 0),
        (sys::SUPPORTS_INPUT_METER, 0),
        (sys::SUPPORTS_OUTPUT_GAIN, 0),
        (sys::SUPPORTS_OUTPUT_METER, 0),
        (99, 0),
    ] {
        assert_eq!(
            device.ask_host(sys::SELECTOR_SUPPORTED, selector),
            Some(supported),
            "selector {selector}"
        );
    }
    assert_eq!(device.ask_host(sys::ENGINE_VERSION, 0), Some(2));
    assert_eq!(device.ask_host(sys::SUPPORTS_TIME_INFO, 0), Some(1));
    assert_eq!(device.ask_host(sys::SUPPORTS_TIME_CODE, 0), Some(0));
    assert_eq!(device.ask_host(sys::SUPPORTS_INPUT_MONITOR, 0), Some(0));
    assert_eq!(device.ask_host(1_000, 0), Some(0));
}

#[test]
fn a_stream_is_checked_before_the_driver_is_touched() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Picky");
    let driver = device.load().unwrap();
    for (config, what) in [
        (StreamConfig::new(), "no channels"),
        (StreamConfig::new().with_outputs([2]), "a missing channel"),
        (StreamConfig::new().with_outputs([1, 1]), "a channel twice"),
        (duplex().with_buffer_size(500), "a size not offered"),
        (duplex().with_sample_rate(22_050.0), "a rate not offered"),
    ] {
        assert!(
            matches!(driver.start(&config, through), Err(Error::InvalidConfig(_))),
            "{what}"
        );
    }
    assert_eq!(device.buffer_creations(), 0);

    let stream = driver
        .start(
            &duplex().with_buffer_size(128).with_sample_rate(96_000.0),
            through,
        )
        .unwrap();
    assert_eq!(
        (stream.buffer_size(), stream.sample_rate()),
        (128, 96_000.0)
    );
    assert_eq!(device.sample_rate(), 96_000.0);
    assert_eq!(driver.start(&duplex(), through).unwrap_err(), Error::Busy);
    let active = driver.channel_info(Direction::Output, 1).unwrap();
    assert!(active.is_active);

    drop(stream);
    assert_eq!(
        (
            device.starts(),
            device.stops(),
            device.buffer_creations(),
            device.buffer_disposals()
        ),
        (1, 1, 1, 1)
    );
    assert!(!device.switch(), "nothing runs after the stream is dropped");
    let again = driver.start(&StreamConfig::new().with_outputs([0]), through);
    assert!(again.is_ok());
}

#[test]
fn a_running_clock_drives_the_stream_by_itself() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Clocked");
    let driver = device.load().unwrap();
    let stream = driver.start(&duplex(), through).unwrap();
    let clock = device.run_clock(Duration::from_millis(1));
    assert!(eventually(|| stream.callbacks() >= 20));
    drop(clock);
    drop(stream);
    assert!(!device.is_running());
}

#[test]
fn a_callback_that_panics_leaves_the_stream_silent() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Panicky");
    let driver = device.load().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let stream = driver
        .start(&duplex(), move |_, output, _| {
            counted.fetch_add(1, Ordering::Relaxed);
            output.channel_mut(0).fill(0.5);
            if counted.load(Ordering::Relaxed) == 2 {
                panic!("the callback's own mistake");
            }
        })
        .unwrap();
    assert!(device.switch());
    assert_ne!(device.read_output(0).unwrap(), vec![0u8; 256 * 4]);
    assert!(device.switch());
    assert!(stream.callback_panicked());
    assert!(device.switch());
    assert_eq!(calls.load(Ordering::Relaxed), 2, "not called again");
    assert_eq!(device.read_output(0).unwrap(), vec![0u8; 256 * 4]);
    assert_eq!(stream.callbacks(), 3);
}
