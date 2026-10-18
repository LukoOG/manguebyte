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
