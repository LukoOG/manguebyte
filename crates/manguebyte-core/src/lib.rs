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
//! - [`decibels`]: gain conversions.

mod atomic;
mod buffer;
pub mod decibels;
mod sample;
mod smoothing;
pub mod spsc;

pub use atomic::{AtomicF32, AtomicF64};
pub use buffer::AudioBuffer;
pub use sample::Sample;
pub use smoothing::Smoothed;
