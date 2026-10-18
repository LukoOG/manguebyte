//! Lists the installed drivers, opens each one, runs about two seconds of
//! silence on all its channels, and reports what it saw: block size,
//! callbacks per second, latencies, and the resets the driver asked for.
//!
//! ```text
//! cargo run -p manguebyte-pro-io --example asio_probe
//! ```
//!
//! Nothing audible is played: the output buffers stay silent. A driver that
//! asks for a reset is closed and opened again, as a host must, and the run
//! goes on until its two seconds are up.
//!
//! ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use manguebyte_pro_io::{Driver, DriverEntry, Notifications, StreamConfig, drivers};

const RUN: Duration = Duration::from_secs(2);
/// A driver that asks for more resets than this is given up on.
const MOST_OPENS: usize = 5;

fn milliseconds(frames: u32, rate: f64) -> f64 {
    f64::from(frames) * 1_000.0 / rate
}

/// The driver's own time of the first and the last block it handed over,
/// and how many there were: the steady rate, free of the start-up.
struct Clock {
    first: AtomicU64,
    last: AtomicU64,
    blocks: AtomicU64,
}

impl Clock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            first: AtomicU64::new(u64::MAX),
            last: AtomicU64::new(0),
            blocks: AtomicU64::new(0),
        })
    }

    /// Blocks a second between the first and the last block.
    fn rate(&self) -> Option<f64> {
        let (first, last) = (
            self.first.load(Ordering::Relaxed),
            self.last.load(Ordering::Relaxed),
        );
        let blocks = self.blocks.load(Ordering::Relaxed);
        (first != u64::MAX && last > first && blocks > 1)
            .then(|| (blocks - 1) as f64 * 1e9 / (last - first) as f64)
    }
}

#[derive(Default)]
struct Totals {
    opens: usize,
    callbacks: u64,
    running: Duration,
    resets: Vec<String>,
    block: u32,
    rate: f64,
    latencies: (u32, u32),
    time_info: bool,
    output_ready: bool,
    skipped: u64,
    steady_rate: Option<f64>,
    first_block: Vec<Duration>,
}

fn describe_resets(notifications: &Notifications) -> String {
    let mut parts = Vec::new();
    if notifications.reset_requests > 0 {
        parts.push(format!("{} reset request(s)", notifications.reset_requests));
    }
    if notifications.resync_requests > 0 {
        parts.push(format!(
            "{} resync request(s)",
            notifications.resync_requests
        ));
    }
    if notifications.buffer_size_changes > 0 {
        parts.push(format!(
            "buffer size change to {:?}",
            notifications.new_buffer_size
        ));
    }
    if notifications.sample_rate_changes > 0 {
        parts.push(format!(
            "sample rate change to {:?}",
            notifications.new_sample_rate
        ));
    }
    parts.join(", ")
}

fn probe(entry: &DriverEntry) -> Result<Totals, manguebyte_pro_io::Error> {
    let mut totals = Totals::default();
    while totals.running < RUN && totals.opens < MOST_OPENS {
        let opened = Instant::now();
        let driver = Driver::load(entry)?;
        totals.opens += 1;
        let info = driver.info()?;
        if totals.opens == 1 {
            println!("  driver: {} (version {})", info.name, info.version);
            println!(
                "  channels: {} in, {} out",
                info.channels.inputs, info.channels.outputs
            );
            println!(
                "  buffer sizes: {} to {}, preferred {}, granularity {}",
                info.buffer_sizes.min,
                info.buffer_sizes.max,
                info.buffer_sizes.preferred,
                info.buffer_sizes.granularity
            );
            println!(
                "  sample rate: {} Hz; accepts {:?}",
                info.sample_rate, info.sample_rates
            );
            let mut types: Vec<String> = info
                .inputs
                .iter()
                .chain(&info.outputs)
                .map(|channel| channel.sample_type.to_string())
                .collect();
            types.dedup();
            println!("  sample types: {}", types.join(", "));
            if let (Some(first), Some(last)) = (info.outputs.first(), info.outputs.last()) {
                println!("  outputs: \"{}\" .. \"{}\"", first.name, last.name);
            }
            let clocks: Vec<&str> = info
                .clock_sources
                .iter()
                .map(|clock| clock.name.as_str())
                .collect();
            println!("  clocks: {clocks:?}");
            println!(
                "  time info offered: {}, overload reports: {}, internal buffering: {:?}",
                info.can_time_info, info.can_report_overload, info.internal_buffering
            );
        }
        let config = StreamConfig::new()
            .with_inputs(0..info.channels.inputs)
            .with_outputs(0..info.channels.outputs);
        // Silence: the stream hands the callback a silent output buffer,
        // and the callback leaves it so. It only reads the driver's clock.
        let clock = Clock::new();
        let timing = Arc::clone(&clock);
        let stream = driver.start(&config, move |_input, _output, info| {
            if let Some(time) = info.system_time {
                let nanos = time.as_nanos() as u64;
                let _ = timing.first.compare_exchange(
                    u64::MAX,
                    nanos,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
                timing.last.store(nanos, Ordering::Relaxed);
            }
            timing.blocks.fetch_add(1, Ordering::Relaxed);
        })?;
        let started = Instant::now();
        println!(
            "  open {}: started {} ms after loading",
            totals.opens,
            (started - opened).as_millis()
        );
        let budget = RUN - totals.running;
        let mut first_block = None;
        while started.elapsed() < budget && !stream.needs_restart() {
            if first_block.is_none() && stream.callbacks() > 0 {
                first_block = Some(started.elapsed());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let ran = started.elapsed();
        if let Some(first) = first_block {
            totals.first_block.push(first);
        }
        if let Some(rate) = clock.rate() {
            totals.steady_rate = Some(rate);
        }
        totals.running += ran;
        totals.callbacks += stream.callbacks();
        totals.block = stream.buffer_size();
        totals.rate = stream.sample_rate();
        let latencies = stream.latencies();
        totals.latencies = (latencies.input, latencies.output);
        totals.time_info |= stream.uses_time_info();
        totals.output_ready |= stream.uses_output_ready();
        totals.skipped += stream.skipped();
        let notifications = stream.notifications();
        if notifications.overloads > 0 {
            println!("  {} overload(s) reported", notifications.overloads);
        }
        if stream.needs_restart() {
            let what = describe_resets(&notifications);
            println!(
                "  the driver asked for a restart after {} ms and {} callbacks: {what}",
                ran.as_millis(),
                stream.callbacks()
            );
            totals.resets.push(what);
        }
        // Stream first, then driver: closed, to be opened again.
        drop(stream);
        drop(driver);
    }
    Ok(totals)
}

fn main() {
    let entries = match drivers() {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("could not list the drivers: {error}");
            std::process::exit(1);
        }
    };
    if entries.is_empty() {
        println!("No drivers are installed.");
        return;
    }
    for entry in &entries {
        println!("{} ({})", entry.description, entry.clsid);
        if let Some(library) = &entry.library {
            println!("  library: {}", library.display());
        }
        match probe(entry) {
            Ok(totals) if totals.running > Duration::ZERO => {
                let seconds = totals.running.as_secs_f64();
                let steady = totals
                    .steady_rate
                    .map_or(String::from("unknown"), |rate| format!("{rate:.1}"));
                println!(
                    "  result: {} frames a block at {} Hz; {} callbacks in {:.2} s of \
                     streaming over {} open(s): {:.1} a second overall, {steady} steady, \
                     {:.1} expected",
                    totals.block,
                    totals.rate,
                    totals.callbacks,
                    seconds,
                    totals.opens,
                    totals.callbacks as f64 / seconds,
                    totals.rate / f64::from(totals.block.max(1)),
                );
                let first: Vec<u128> = totals.first_block.iter().map(Duration::as_millis).collect();
                println!("  first callback after start, in ms, per open: {first:?}");
                println!(
                    "  latencies: {} frames in ({:.2} ms), {} frames out ({:.2} ms)",
                    totals.latencies.0,
                    milliseconds(totals.latencies.0, totals.rate),
                    totals.latencies.1,
                    milliseconds(totals.latencies.1, totals.rate)
                );
                println!(
                    "  bufferSwitchTimeInfo: {}, outputReady: {}, re-entered calls skipped: {}",
                    totals.time_info, totals.output_ready, totals.skipped
                );
                println!("  restarts asked for: {}", totals.resets.len());
            }
            Ok(_) => println!("  result: no stream ran"),
            Err(error) => println!("  result: {error}"),
        }
        println!();
    }
}
