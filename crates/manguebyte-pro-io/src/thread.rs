//! The thread a driver lives on.
//!
//! Drivers register themselves as single-threaded COM objects (the
//! registry's `ThreadingModel` is `Apartment` for most), and the SDK's host
//! code makes the thread that creates one a single-threaded apartment. So
//! each loaded driver gets a thread of its own that does exactly that:
//! every call into the driver except the two the SDK has the host make from
//! inside `bufferSwitch` (`outputReady` and `getSamplePosition`) runs
//! there, whichever thread asked for it, and the thread hands the
//! apartment's window messages on between calls, as an apartment must.
//! Between calls it also runs the stream's housekeeping: noticing what the
//! driver reported through `asioMessage`, off the audio thread.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::Error;
use crate::raw::RawDriver;

/// How often the thread looks at its messages and its housekeeping while
/// nothing is asked of it.
pub(crate) const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(10);

/// The housekeeping a running stream asks of the driver thread.
pub(crate) type Housekeeping = Box<dyn FnMut(&RawDriver) + Send>;

/// What lives on the driver thread.
#[derive(Default)]
pub(crate) struct Apartment {
    /// The driver, once created.
    pub(crate) driver: Option<RawDriver>,
    /// Run every [`HOUSEKEEPING_INTERVAL`] while a stream runs.
    pub(crate) housekeeping: Option<Housekeeping>,
}

type Job = Box<dyn FnOnce(&mut Apartment) + Send>;

pub(crate) struct DriverThread {
    jobs: Mutex<Option<Sender<Job>>>,
    thread: Option<JoinHandle<()>>,
}

impl DriverThread {
    pub(crate) fn spawn() -> Result<Self, Error> {
        let (sender, receiver) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name(String::from("manguebyte-pro-io driver"))
            .spawn(move || {
                #[cfg(windows)]
                let com = crate::com::initialize();
                let mut apartment = Apartment::default();
                loop {
                    match receiver.recv_timeout(HOUSEKEEPING_INTERVAL) {
                        Ok(job) => job(&mut apartment),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    #[cfg(windows)]
                    crate::com::pump_messages();
                    if let (Some(driver), Some(housekeeping)) =
                        (&apartment.driver, &mut apartment.housekeeping)
                    {
                        housekeeping(driver);
                    }
                }
                // The driver is released on the thread that created it,
                // before the apartment closes.
                apartment.housekeeping = None;
                apartment.driver = None;
                #[cfg(windows)]
                if com {
                    crate::com::uninitialize();
                }
            })
            .map_err(|_| Error::ThreadGone)?;
        Ok(Self {
            jobs: Mutex::new(Some(sender)),
            thread: Some(thread),
        })
    }

    /// Runs `job` on the driver thread and waits for what it returns. A job
    /// that panics is reported as [`Error::ThreadGone`], and the driver is
    /// not touched again.
    pub(crate) fn run<R, F>(&self, job: F) -> Result<R, Error>
    where
        R: Send + 'static,
        F: FnOnce(&mut Apartment) -> R + Send + 'static,
    {
        let (reply, answer) = mpsc::sync_channel(1);
        let job: Job = Box::new(move |apartment: &mut Apartment| {
            match catch_unwind(AssertUnwindSafe(|| job(apartment))) {
                Ok(result) => {
                    let _ = reply.send(result);
                }
                Err(_) => {
                    // Leave the driver alone from now on: forget it rather
                    // than release it half way through whatever failed.
                    std::mem::forget(apartment.driver.take());
                    apartment.housekeeping = None;
                }
            }
        });
        {
            let jobs = self.jobs.lock().map_err(|_| Error::ThreadGone)?;
            jobs.as_ref()
                .ok_or(Error::ThreadGone)?
                .send(job)
                .map_err(|_| Error::ThreadGone)?;
        }
        answer.recv().map_err(|_| Error::ThreadGone)
    }

    /// Runs `job` with the driver, which must have been created.
    pub(crate) fn with_driver<R, F>(&self, job: F) -> Result<R, Error>
    where
        R: Send + 'static,
        F: FnOnce(&RawDriver) -> Result<R, Error> + Send + 'static,
    {
        self.run(move |apartment| match &apartment.driver {
            Some(driver) => job(driver),
            None => Err(Error::ThreadGone),
        })?
    }
}

impl Drop for DriverThread {
    fn drop(&mut self) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.take();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
