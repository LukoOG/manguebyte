//! Core building blocks of the Manguebyte audio framework.
//!
//! This crate has no dependencies and holds the types every other Manguebyte
//! crate is built on:
//!
//! - [`Sample`]: the floating-point sample abstraction (`f32` and `f64`).
//! - [`AudioBuffer`]: a planar, multi-channel block of audio.
//! - [`Smoothed`]: click-free parameter ramps.
//! - [`AtomicF32`] / [`AtomicF64`]: lock-free values shared with the audio thread.
//! - [`spsc`]: a wait-free single-producer single-consumer queue.
//! - [`log`]: a log that any thread, the audio thread included, can write to.
//! - [`decibels`]: gain conversions.
//! - [`NoDenormals`]: stops near-silence from slowing the processor down.
//! - [`ParamId`]: a compact, stable number standing for a parameter.

mod atomic;
mod buffer;
pub mod decibels;
mod denormal;
pub mod log;
mod param_id;
mod sample;
mod smoothing;
pub mod spsc;

pub use atomic::{AtomicF32, AtomicF64};
pub use buffer::AudioBuffer;
pub use denormal::NoDenormals;
pub use log::{DrainedLog, LogLevel, LogQueue, LogRecord};
pub use param_id::ParamId;
pub use sample::Sample;
pub use smoothing::Smoothed;
