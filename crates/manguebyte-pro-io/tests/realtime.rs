//! Proves that the path from a driver's `bufferSwitch` to the callback
//! never touches the heap: the host callbacks, the sample conversion, the
//! time and `outputReady`, and, behind a device manager, the backend's
//! block splitting and the manager's engine.
//!
//! The fake driver calls the host's real callbacks from the test's thread,
//! exactly as a driver's thread would, while a counting allocator watches.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::{Mutex, MutexGuard};

use manguebyte_devices::{DeviceChoice, DeviceIo, DeviceManager, DeviceSetup};
use manguebyte_pro_io::{BufferSizes, FakeDevice, ProIoBackend, SampleType, StreamConfig};

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

/// Runs `work` and panics if it used the heap in any way.
fn assert_no_heap_use(what: &str, work: impl FnOnce()) {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    work();
    WATCHING.with(|watching| watching.set(false));
    let calls = HEAP_CALLS.with(Cell::get);
    assert_eq!(
        calls, 0,
        "{what} used the heap {calls} time(s) while processing"
    );
}

fn one_at_a_time() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn the_guard_itself_catches_an_allocation() {
    let caught = std::panic::catch_unwind(|| {
        assert_no_heap_use("a Vec", || drop(std::hint::black_box(vec![0u8; 64])));
    });
    WATCHING.with(|watching| watching.set(false));
    assert!(caught.is_err(), "the allocation guard is not working");
}

#[test]
fn a_stream_does_not_use_the_heap_in_any_sample_type_or_mode() {
    let _turn = one_at_a_time();
    for sample_type in SampleType::ALL.into_iter().filter(|ty| ty.is_supported()) {
        for (time_info, output_ready) in [(true, false), (false, true)] {
            let device = FakeDevice::new("Realtime")
                .with_channels(2, 2, sample_type)
                .with_time_info(time_info)
                .with_output_ready(output_ready);
            let driver = device.load().unwrap();
            let config = StreamConfig::new().with_inputs(0..2).with_outputs(0..2);
            let stream = driver
                .start(&config, |input, output, info| {
                    for channel in 0..2 {
                        for (out, sample) in output
                            .channel_mut(channel)
                            .iter_mut()
                            .zip(input.channel(channel))
                        {
                            *out = sample * 0.5 + info.frame as f32 * 1e-9;
                        }
                    }
                })
                .unwrap();
            let what = format!("{sample_type}, time info {time_info}");
            assert_no_heap_use(&what, || {
                for _ in 0..8 {
                    assert!(device.switch());
                }
            });
            assert_eq!(stream.callbacks(), 8, "{what}");
        }
    }
}

#[test]
fn a_device_manager_on_a_driver_does_not_use_the_heap() {
    let _turn = one_at_a_time();
    // A driver block four times the callback's, which the backend splits.
    let device = FakeDevice::new("Managed").with_buffer_sizes(BufferSizes {
        min: 1_024,
        max: 1_024,
        preferred: 1_024,
        granularity: 0,
    });
    let mut manager =
        DeviceManager::with_audio_backend(ProIoBackend::with_fake_drivers([device.clone()]));
    manager.set_max_block_size(256);
    let setup = DeviceSetup::default().with_input(DeviceChoice::Default);
    manager
        .start(&setup, |io: &mut DeviceIo<'_>| {
            io.read_midi(|_, _| {});
            let frames = io.num_frames();
            for channel in 0..io.output.num_channels() {
                let input = io.input.channel(channel % io.input.num_channels());
                for (out, sample) in io.output.channel_mut(channel)[..frames]
                    .iter_mut()
                    .zip(input)
                {
                    *out = *sample + io.info.frame as f32 * 1e-9;
                }
            }
        })
        .unwrap();
    assert_no_heap_use("a device manager on a driver", || {
        for _ in 0..8 {
            assert!(device.switch());
        }
    });
}
