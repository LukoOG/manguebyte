//! Plugin hosting for the Manguebyte audio framework.
//!
//! This crate loads plugins made by anyone and runs them as
//! [`Processor`](manguebyte_dsp::Processor)s, so a hosted plugin goes
//! wherever one of the framework's own processors goes: a device callback,
//! a chain, a graph, an offline render.
//!
//! Each plugin format is a module behind a feature of the same name, and
//! one module speaks for both:
//!
//! - [`clap`]: CLAP plugins.
//! - [`vst3`]: VST 3 plugins.
//! - [`lv2`]: LV2 plugins, read from their bundles' Turtle files.
//! - [`any`]: any of them through the same types, for an application
//!   that lists, loads, restores and runs plugins without caring which
//!   format each one is.
//!
//! Whatever the format, a hosted plugin comes in two halves. The half that
//! loads, activates, lists parameters and saves state stays on the thread
//! that created it, and the compiler keeps it there. Activating it yields
//! the half that processes audio, which can be sent to the audio thread.
//! While audio is processed the two talk through wait-free queues and
//! never through a lock.
//!
//! Loading a plugin is `unsafe`: it runs code this crate cannot check, and
//! the caller vouches for it. Everything after that is safe to call.

#[cfg(any(feature = "clap", feature = "vst3", feature = "lv2"))]
pub mod any;
#[cfg(any(feature = "clap", feature = "vst3"))]
mod bundle;
#[cfg(feature = "clap")]
pub mod clap;
#[cfg(feature = "lv2")]
pub mod lv2;
#[cfg(any(feature = "clap", feature = "vst3", feature = "lv2"))]
mod threads;
#[cfg(feature = "vst3")]
pub mod vst3;
