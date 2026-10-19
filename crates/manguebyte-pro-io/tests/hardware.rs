//! Tests that need installed drivers and the hardware behind them.
//!
//! They are ignored by default because CI machines have none. Run them on
//! a machine that does with:
//!
//! ```text
//! cargo test -p manguebyte-pro-io --test hardware -- --ignored --test-threads 1 --nocapture
//! ```
//!
//! Nothing audible is played: every output buffer stays silent.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use manguebyte_devices::{
    DeviceChoice, DeviceEvent, DeviceIo, DeviceManager, DeviceRef, DeviceSetup, Direction,
    LossCause,
};
use manguebyte_pro_io::{Driver, ID_PREFIX, ProIoBackend, StreamConfig, drivers};

const RUN: Duration = Duration::from_secs(2);

#[test]
#[ignore = "needs installed drivers"]
fn the_installed_drivers_are_listed_with_their_classes() {
    let entries = drivers().unwrap();
    for entry in &entries {
        println!("{entry:?}");
        assert!(!entry.name.is_empty());
        assert!(entry.library.as_ref().is_some_and(|path| path.exists()));
        assert_eq!(
            entry.clsid.to_string().parse::<manguebyte_pro_io::Clsid>(),
            Ok(entry.clsid)
        );
    }
    assert!(!entries.is_empty(), "no drivers are installed");
}

#[test]
#[ignore = "needs installed drivers"]
fn every_driver_runs_two_seconds_of_silence_and_survives_its_resets() {
    for entry in drivers().unwrap() {
        let mut streamed = Duration::ZERO;
        let mut callbacks = 0;
        let mut opens = 0;
        let mut resets = 0;
        let mut steady = None;
        while streamed < RUN && opens < 5 {
            let driver = match Driver::load(&entry) {
                Ok(driver) => driver,
                Err(error) => panic!("{}: {error}", entry.name),
            };
            opens += 1;
            let channels = driver.channels().unwrap();
            let config = StreamConfig::new()
                .with_inputs(0..channels.inputs)
                .with_outputs(0..channels.outputs);
            let first = Arc::new(AtomicU64::new(u64::MAX));
            let last = Arc::new(AtomicU64::new(0));
            let (seen_first, seen_last) = (Arc::clone(&first), Arc::clone(&last));
            let stream = driver
                .start(&config, move |_, output, info| {
                    // Silent in, silent out.
                    assert_eq!(output.peak(), 0.0);
                    if let Some(time) = info.system_time {
                        let nanos = time.as_nanos() as u64;
                        let _ = seen_first.compare_exchange(
                            u64::MAX,
                            nanos,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        );
                        seen_last.store(nanos, Ordering::Relaxed);
                    }
                })
                .unwrap();
            let started = Instant::now();
            while started.elapsed() < RUN - streamed && !stream.needs_restart() {
                std::thread::sleep(Duration::from_millis(5));
            }
            streamed += started.elapsed();
            callbacks += stream.callbacks();
            if stream.needs_restart() {
                resets += 1;
                println!(
                    "{}: restart asked for after {} callbacks: {:?}",
                    entry.name,
                    stream.callbacks(),
                    stream.notifications()
                );
            }
            let (first, last) = (first.load(Ordering::Relaxed), last.load(Ordering::Relaxed));
            if first != u64::MAX && last > first && stream.callbacks() > 1 {
                steady = Some((stream.callbacks() - 1) as f64 * 1e9 / (last - first) as f64);
            }
            let expected = stream.sample_rate() / f64::from(stream.buffer_size());
            println!(
                "{}: {} frames at {} Hz, latencies {:?}, {} callbacks, steady {:?} a second \
                 (expected {expected:.1}), time info {}, outputReady {}",
                entry.name,
                stream.buffer_size(),
                stream.sample_rate(),
                stream.latencies(),
                stream.callbacks(),
                steady,
                stream.uses_time_info(),
                stream.uses_output_ready()
            );
            if let Some(steady) = steady {
                assert!(
                    (steady / expected - 1.0).abs() < 0.1,
                    "{}: {steady} callbacks a second, {expected} expected",
                    entry.name
                );
            }
        }
        println!(
            "{}: {callbacks} callbacks in {:.2} s over {opens} open(s), {resets} restart(s)",
            entry.name,
            streamed.as_secs_f64()
        );
        assert!(callbacks > 0, "{}: the driver never called", entry.name);
        assert!(streamed >= RUN, "{}: kept asking for resets", entry.name);
    }
}

/// Runs an engine of silence through the device manager and changes the
/// driver under it, FlexASIO to ASIO4ALL and back. ASIO loads one driver
/// per process, so each change fades out, closes one driver, loads the
/// other and fades in; the engine is never prepared again and its frames
/// follow on. Prints the silence each change left.
#[test]
#[ignore = "needs FlexASIO and ASIO4ALL"]
fn the_engine_runs_on_while_the_driver_changes() {
    use manguebyte_devices::{AudioCallback, EngineFormat, StreamFormat};

    /// Leaves every block silent, checks that the frames follow on, and
    /// keeps the longest time between two blocks.
    struct Silence {
        prepared: Arc<AtomicU64>,
        frames: Arc<AtomicU64>,
        wrong: Arc<AtomicU64>,
        origin: Instant,
        last: u64,
        longest: Arc<AtomicU64>,
    }

    impl AudioCallback for Silence {
        fn prepare(&mut self, _: &StreamFormat) {
            self.prepared.fetch_add(1, Ordering::Relaxed);
        }

        fn process(&mut self, io: &mut DeviceIo<'_>) {
            let frames = io.num_frames() as u64;
            let expected = self.frames.fetch_add(frames, Ordering::Relaxed);
            if io.info.frame != expected || io.output.peak() != 0.0 {
                self.wrong.fetch_add(1, Ordering::Relaxed);
            }
            let now = self.origin.elapsed().as_nanos() as u64;
            if self.last > 0 {
                self.longest.fetch_max(now - self.last, Ordering::Relaxed);
            }
            self.last = now;
        }
    }

    let entries = drivers().unwrap();
    let find = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.name.to_lowercase().contains(name))
            .cloned()
    };
    let (Some(flex), Some(asio4all)) = (find("flexasio"), find("asio4all")) else {
        println!("FlexASIO or ASIO4ALL is not installed: {entries:?}");
        return;
    };
    let setup = |entry: &manguebyte_pro_io::DriverEntry| {
        let device = DeviceChoice::Device(DeviceRef::new(
            format!("{ID_PREFIX}{}", entry.name),
            entry.description.clone(),
        ));
        DeviceSetup::default().with_output(device)
    };
    let prepared = Arc::new(AtomicU64::new(0));
    let frames = Arc::new(AtomicU64::new(0));
    let wrong = Arc::new(AtomicU64::new(0));
    let longest = Arc::new(AtomicU64::new(0));
    let mut manager = DeviceManager::with_audio_backend(ProIoBackend::new());
    // Described now, as a settings dialog would, while no driver is
    // loaded: a driver cannot be described while another one runs.
    for entry in [&flex, &asio4all] {
        let id = format!("{ID_PREFIX}{}", entry.name);
        manager.device_info(Direction::Output, &id).unwrap();
    }
    let silence = Silence {
        prepared: Arc::clone(&prepared),
        frames: Arc::clone(&frames),
        wrong: Arc::clone(&wrong),
        origin: Instant::now(),
        last: 0,
        longest: Arc::clone(&longest),
    };
    let format = EngineFormat::new(48_000, 1_024, 0, 2);
    manager
        .start_engine(&setup(&flex), format, silence)
        .unwrap();
    let run = |manager: &mut DeviceManager, what: &str| {
        let before = frames.load(Ordering::Relaxed);
        let started = Instant::now();
        // Until the new driver has played, and some more.
        while started.elapsed() < RUN
            || (manager.last_swap().unwrap().silence.is_none()
                && started.elapsed() < Duration::from_secs(5))
        {
            for event in manager.poll_devices() {
                println!("{what}: {event}");
                if let Some(result) = manager.follow(&event) {
                    result.unwrap();
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let swap = manager.last_swap().unwrap();
        println!(
            "{what}: {:?} on {:?} at {} Hz, {:?} of silence, closed and loaded in {:?}, {} engine \
             frames since, at most {:?} between two blocks",
            swap.kind,
            manager.running_device(Direction::Output).map(|d| &d.name),
            manager.format().unwrap().sample_rate,
            swap.silence,
            swap.reopen,
            frames.load(Ordering::Relaxed) - before,
            Duration::from_nanos(longest.load(Ordering::Relaxed))
        );
        assert!(frames.load(Ordering::Relaxed) > before, "{what}");
    };
    run(&mut manager, "FlexASIO");
    for (entry, what) in [(&asio4all, "to ASIO4ALL"), (&flex, "back to FlexASIO")] {
        let started = Instant::now();
        longest.store(0, Ordering::Relaxed);
        manager.apply(&setup(entry)).unwrap();
        println!("{what}: apply returned after {:?}", started.elapsed());
        run(&mut manager, what);
    }
    assert_eq!(prepared.load(Ordering::Relaxed), 1);
    assert_eq!(wrong.load(Ordering::Relaxed), 0);
}

#[test]
#[ignore = "needs installed drivers"]
fn every_driver_runs_duplex_through_the_device_manager() {
    for entry in drivers().unwrap() {
        let id = format!("{ID_PREFIX}{}", entry.name);
        let mut manager = DeviceManager::with_audio_backend(ProIoBackend::new());
        let device = DeviceChoice::Device(DeviceRef::new(id.clone(), entry.description.clone()));
        let setup = DeviceSetup::default()
            .with_output(device.clone())
            .with_input(device);
        let blocks = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&blocks);
        let report = manager
            .start(&setup, move |io: &mut DeviceIo<'_>| {
                // The output is left as it came: silent.
                let _ = io.input.num_channels();
                counted.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap();
        assert!(report.is_exact(), "{}: {:?}", entry.name, report.fallbacks);
        assert_eq!(manager.running_device(Direction::Input).unwrap().id, id);
        let format = manager.format().unwrap();
        let started = Instant::now();
        let mut restarts = 0;
        while started.elapsed() < RUN {
            for event in manager.poll_devices() {
                if let DeviceEvent::StreamLost { cause, .. } = &event {
                    println!("{}: {event}", entry.name);
                    assert_eq!(*cause, LossCause::RestartRequested, "{}", entry.name);
                    restarts += 1;
                }
                if let Some(result) = manager.follow(&event) {
                    result.unwrap();
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        println!(
            "{}: {} in, {} out at {} Hz, {} blocks of at most {}, {restarts} restart(s)",
            entry.name,
            format.input_channels,
            format.output_channels,
            format.sample_rate,
            blocks.load(Ordering::Relaxed),
            format.max_block_size
        );
        assert!(manager.is_running(), "{}", entry.name);
        assert!(blocks.load(Ordering::Relaxed) > 0, "{}", entry.name);
        manager.stop();
    }
}
