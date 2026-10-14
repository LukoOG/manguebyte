//! What the two halves of a hosted VST 3 plugin have in common.
//!
//! Both halves hold a [`Shared`] in an `Arc`. Everything in it that changes
//! while audio runs is an atomic, so the audio half never waits for the
//! main half. The two locks in it are taken on the main thread, and by the
//! audio half only as it is dropped, which is not a real-time operation.
//!
//! [`Shared`] also owns the plugin's objects, and is what terminates and
//! releases them: on the main thread, once neither half needs them, which
//! is when VST 3 allows it.
//!
//! # Generations
//!
//! Parameter values are on their way between the halves all the time: from
//! the main thread to the audio half, from the audio half into the plugin's
//! processor, and back to the main thread to be told to the controller.
//! Loading a state replaces all of them at once. So every value carries the
//! generation it was sent in, which is the number of states loaded before
//! it, and a value from an older generation than the current one is
//! dropped wherever it is found instead of being passed on.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use manguebyte_core::spsc::Consumer;
use vst3::Steinberg::IPluginBaseTrait;
use vst3::Steinberg::Vst::{
    IAudioProcessor, IComponent, IComponentTrait, IConnectionPoint, IConnectionPointTrait,
    IEditController, IEditControllerTrait, IMidiMapping,
};
use vst3::{ComPtr, ComWrapper};

use super::objects::{ComponentHandler, HostApplication, lock};
use super::{ProcessFailure, ToAudio, Vst3Module};
use crate::threads::thread_token;

/// The plugin's objects, and what they were given, in the order they are
/// released.
///
/// The halves call the plugin through plain pointers to these objects; the
/// references held here are what keeps those pointers valid. Some fields
/// are only held, never read, for that reason.
pub(crate) struct Objects {
    /// The component's and the controller's connection points, connected
    /// to each other, when the two are separate objects.
    pub(crate) connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    pub(crate) _midi_mapping: Option<ComPtr<IMidiMapping>>,
    pub(crate) controller: Option<ComPtr<IEditController>>,
    /// Whether the controller is an object of its own, which is
    /// initialised and terminated by itself.
    pub(crate) separate_controller: bool,
    pub(crate) _processor: ComPtr<IAudioProcessor>,
    pub(crate) component: ComPtr<IComponent>,
    /// What the controller reports to. A plugin that keeps a reference to
    /// it past termination keeps it alive by itself.
    pub(crate) _handler: ComWrapper<ComponentHandler>,
    /// What the plugin was given as its host context.
    pub(crate) _host: ComWrapper<HostApplication>,
    /// Keeps the plugin's code loaded. Released last.
    pub(crate) _module: Vst3Module,
}

/// What both halves of a hosted plugin have in common.
pub(crate) struct Shared {
    /// The thread the plugin was created on, as its [`thread_token`].
    main_thread: u64,
    active: AtomicBool,
    processor_alive: AtomicBool,
    latency: AtomicU32,
    /// The plugin's tail as VST 3 reports it (`getTailSamples`), as the
    /// main thread last read it. VST 3 lets only that thread ask.
    tail: AtomicU32,
    /// The current generation of parameter values: how many states have
    /// been loaded. See the module documentation.
    generation: AtomicU64,
    /// Kinds of processing failure seen since the main thread last asked.
    failures: AtomicU32,
    /// Blocks that ended in silence because the plugin failed.
    pub(crate) failed_blocks: AtomicU64,
    /// Events and points dropped because a list or a queue was full.
    pub(crate) lost_events: AtomicU64,
    /// Events not sent because the plugin has no way to receive them.
    pub(crate) undeliverable_events: AtomicU64,
    /// The receiving end of the main thread's queue, handed back by the
    /// audio half when it is dropped, with whatever it had not read.
    leftover: Mutex<Option<Consumer<ToAudio>>>,
    /// Set when the main handle has been dropped.
    main_gone: AtomicBool,
    /// Set when the plugin has been terminated, or is about to be.
    closed: AtomicBool,
    /// The plugin's objects, until they are terminated and released.
    objects: Mutex<Option<Objects>>,
}

// Both halves hold a `Shared`, on different threads, so it has to be safe
// to share. Nothing here needs to promise that by hand: every field that
// changes is an atomic or behind a mutex. This fails to compile if a field
// is ever added for which that is not true.
const _: () = {
    const fn shareable<T: Send + Sync>() {}
    shareable::<Shared>();
};

impl Shared {
    /// Takes charge of a plugin's objects. The calling thread is the
    /// plugin's main thread.
    pub(crate) fn new(objects: Objects) -> Arc<Self> {
        Arc::new(Self {
            main_thread: thread_token(),
            active: AtomicBool::new(false),
            processor_alive: AtomicBool::new(false),
            latency: AtomicU32::new(0),
            tail: AtomicU32::new(0),
            generation: AtomicU64::new(0),
            failures: AtomicU32::new(0),
            failed_blocks: AtomicU64::new(0),
            lost_events: AtomicU64::new(0),
            undeliverable_events: AtomicU64::new(0),
            leftover: Mutex::new(None),
            main_gone: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            objects: Mutex::new(Some(objects)),
        })
    }

    /// Whether the calling thread is the plugin's main thread.
    pub(crate) fn is_main(&self) -> bool {
        let token = thread_token();
        token != 0 && token == self.main_thread
    }

    /// Records that a block ended in silence, and why. Real-time safe.
    pub(crate) fn fail(&self, failure: ProcessFailure) {
        self.failures.fetch_or(failure.bit(), Ordering::Relaxed);
        self.failed_blocks.fetch_add(1, Ordering::Relaxed);
    }

    /// The kinds of failure seen since the last call, as
    /// [`ProcessFailure::bit`]s.
    pub(crate) fn take_failures(&self) -> u32 {
        self.failures.swap(0, Ordering::Relaxed)
    }

    pub(crate) fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }

    pub(crate) fn processor_alive(&self) -> bool {
        self.processor_alive.load(Ordering::Acquire)
    }

    /// Records that an audio half now exists.
    pub(crate) fn processor_created(&self) {
        self.processor_alive.store(true, Ordering::Release);
    }

    /// Called by the audio half as it is dropped: hands back its end of the
    /// main thread's queue, then lets the main thread know it is gone.
    ///
    /// The order matters. A value the main thread queues while it still
    /// believes the audio half is alive is then certain to be in the queue
    /// that was handed back.
    pub(crate) fn processor_dropped(&self, unread: Option<Consumer<ToAudio>>) {
        *lock(&self.leftover) = unread;
        self.processor_alive.store(false, Ordering::SeqCst);
    }

    /// The queue end handed back by a dropped audio half, if any.
    pub(crate) fn take_leftover(&self) -> Option<Consumer<ToAudio>> {
        lock(&self.leftover).take()
    }

    pub(crate) fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    pub(crate) fn set_latency(&self, frames: u32) {
        self.latency.store(frames, Ordering::Relaxed);
    }

    /// The tail as the main thread last read it, in VST 3's terms.
    /// Real-time safe.
    pub(crate) fn tail(&self) -> u32 {
        self.tail.load(Ordering::Relaxed)
    }

    pub(crate) fn set_tail(&self, raw: u32) {
        self.tail.store(raw, Ordering::Relaxed);
    }

    /// The current generation of parameter values. Real-time safe.
    ///
    /// The main thread starts a generation before it sends anything in it,
    /// and the queue between the halves orders what is sent. So the audio
    /// half, reading this after it has taken a value from the queue, sees
    /// that value's generation or a later one, and a later one means a
    /// state was loaded after the value was sent.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Starts a new generation: everything sent before now is replaced.
    /// Called on the main thread when a state is loaded.
    pub(crate) fn next_generation(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Deactivates the plugin if it is active, disconnects and terminates
    /// its objects, and releases them. Does nothing the second time.
    ///
    /// A method on a shared reference and not a destructor, for the reason
    /// the CLAP host gives: a plugin may call back into its host while it
    /// is being terminated.
    ///
    /// To be called on the plugin's main thread only, with no audio half in
    /// existence: the two conditions VST 3 sets for terminating a plugin.
    pub(crate) fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(objects) = lock(&self.objects).take() else {
            return;
        };
        // SAFETY: the plugin's own objects, alive until `objects` is
        // dropped at the end of this function. Per this function's
        // contract this is the main thread and nothing is processing, which
        // is what VST 3 asks for every call here. The connection points
        // were connected to each other when the plugin was created.
        unsafe {
            if self.active.swap(false, Ordering::AcqRel) {
                objects.component.setActive(0);
            }
            if let Some((component, controller)) = &objects.connection {
                component.disconnect(controller.as_ptr());
                controller.disconnect(component.as_ptr());
            }
            if let Some(controller) = &objects.controller {
                controller.setComponentHandler(std::ptr::null_mut());
                if objects.separate_controller {
                    controller.terminate();
                }
            }
            objects.component.terminate();
        }
        // Released in the order of the fields, the module last.
        drop(objects);
    }

    /// Called when the main handle is dropped, on the main thread.
    ///
    /// If the audio half is gone too, the plugin is closed now. If not,
    /// that falls to the audio half, in
    /// [`processor_gone`](Self::processor_gone).
    pub(crate) fn main_handle_dropped(&self) {
        // Sequentially consistent, with the mirrored pair in
        // `processor_gone` and `processor_dropped`, so that when both halves
        // go at the same moment on two threads, at least one of them sees
        // that the other has gone.
        self.main_gone.store(true, Ordering::SeqCst);
        if !self.processor_alive.load(Ordering::SeqCst) {
            self.close();
        }
    }

    /// Called by the audio half as the last thing it does, with its own
    /// reference to this value.
    ///
    /// While the main handle exists, closing the plugin is its job. If it
    /// is gone and this happens to be the main thread, the plugin is closed
    /// here. If it is gone and this is another thread, no thread is left on
    /// which VST 3 allows the plugin to be terminated. It is then kept, with
    /// this value and its code, until the process ends: a leak, chosen over
    /// terminating a plugin on a thread it does not expect.
    pub(crate) fn processor_gone(this: &Arc<Self>) {
        if !this.main_gone.load(Ordering::SeqCst) {
            return;
        }
        if this.is_main() {
            this.close();
        } else if !this.closed.load(Ordering::SeqCst) {
            std::mem::forget(Arc::clone(this));
        }
    }
}

/// The last normalized value the audio half handed the plugin for each
/// parameter, for the main thread to tell the controller.
///
/// A VST 3 host is the one that plays automation, and it keeps the
/// controller up to date with what it sends the component, so that what a
/// person sees follows what is heard. The audio half writes here as it
/// sends; the main thread reads in [`Vst3Plugin::pump`](super::Vst3Plugin::pump).
///
/// Each value is kept with its generation, and only values of the current
/// one are handed on: a state loaded since replaced the others.
pub(crate) struct Delivered {
    /// Parameter ids, sorted.
    ids: Box<[u32]>,
    /// The bits of the last value, in the order of `ids`.
    values: Box<[AtomicU64]>,
    /// The generation each value was handed over in.
    generations: Box<[AtomicU64]>,
    /// Whether the value changed since the main thread last looked.
    fresh: Box<[AtomicBool]>,
}

impl Delivered {
    /// A table for these parameters. Allocates.
    pub(crate) fn new(mut ids: Vec<u32>) -> Arc<Self> {
        ids.sort_unstable();
        ids.dedup();
        Arc::new(Self {
            values: ids.iter().map(|_| AtomicU64::new(0)).collect(),
            generations: ids.iter().map(|_| AtomicU64::new(0)).collect(),
            fresh: ids.iter().map(|_| AtomicBool::new(false)).collect(),
            ids: ids.into_boxed_slice(),
        })
    }

    /// Whether `id` is one of the parameters. Real-time safe.
    pub(crate) fn knows(&self, id: u32) -> bool {
        self.ids.binary_search(&id).is_ok()
    }

    /// Records that `id` was handed `value` in `generation`. Real-time
    /// safe.
    pub(crate) fn note(&self, id: u32, value: f64, generation: u64) {
        if let Ok(index) = self.ids.binary_search(&id) {
            self.values[index].store(value.to_bits(), Ordering::Relaxed);
            self.generations[index].store(generation, Ordering::Relaxed);
            self.fresh[index].store(true, Ordering::Release);
        }
    }

    /// Hands every value recorded since the last call to `visit`, as
    /// `(id, value)`, and forgets the ones recorded before `current`, the
    /// current generation.
    ///
    /// The value and its generation are two atomics, so a value recorded
    /// while this runs may be read with the other one's generation. It is
    /// then marked fresh again after this has looked, and the next call
    /// hands on the value that was recorded last, with its own generation.
    pub(crate) fn take(&self, current: u64, mut visit: impl FnMut(u32, f64)) {
        for (index, &id) in self.ids.iter().enumerate() {
            if self.fresh[index].swap(false, Ordering::Acquire)
                && self.generations[index].load(Ordering::Relaxed) >= current
            {
                visit(
                    id,
                    f64::from_bits(self.values[index].load(Ordering::Relaxed)),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delivered_value_is_seen_once_and_only_for_a_known_parameter() {
        let table = Delivered::new(vec![9, 3, 3, 7]);
        assert!(table.knows(3) && table.knows(7) && table.knows(9));
        assert!(!table.knows(4));
        table.note(7, 0.25, 0);
        table.note(3, 0.5, 0);
        table.note(7, 0.75, 0);
        table.note(4, 1.0, 0);
        let mut seen = Vec::new();
        table.take(0, |id, value| seen.push((id, value)));
        assert_eq!(seen, [(3, 0.5), (7, 0.75)]);
        seen.clear();
        table.take(0, |id, value| seen.push((id, value)));
        assert!(seen.is_empty(), "a value was reported twice");
    }

    #[test]
    fn a_value_delivered_before_a_state_was_loaded_is_forgotten() {
        let table = Delivered::new(vec![1, 2]);
        table.note(1, 0.1, 0);
        table.note(2, 0.2, 0);
        // A state was loaded, and then 2 was delivered again.
        table.note(2, 0.3, 1);
        let mut seen = Vec::new();
        table.take(1, |id, value| seen.push((id, value)));
        assert_eq!(seen, [(2, 0.3)]);
        // The older value is gone, not kept for later.
        seen.clear();
        table.take(0, |id, value| seen.push((id, value)));
        assert!(seen.is_empty(), "{seen:?}");
    }
}
