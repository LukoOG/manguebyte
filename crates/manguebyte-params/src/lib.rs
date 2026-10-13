//! Parameters: the values a person, a host or an automation lane can change
//! while audio is running.
//!
//! - [`FloatParam`], [`IntParam`], [`BoolParam`] and [`ChoiceParam`] hold one
//!   value each, and [`Param`] is what they have in common.
//! - [`FloatRange`] shapes how a control feels: linear, skewed or logarithmic.
//! - [`ParamSet`] groups parameters, finds them by identifier, and saves and
//!   restores them as [`ParamState`].
//! - [`ParamSmoother`] gives the audio thread a click-free view of a value.
//!
//! # Threads
//!
//! A parameter stores its value in an atomic and every method takes `&self`,
//! so a set can be shared in an `Arc` between the audio thread, a user
//! interface and a host with no locks and no possibility of a data race.
//! There are deliberately no change listeners: a callback fired from
//! whichever thread happened to write the value is the classic way for
//! user-interface code to end up running on the audio thread. Readers poll
//! instead, with [`ParamSmoother::follow`] or [`Changed`] on the audio thread
//! and a timer on the interface side.
//!
//! Reading and writing values is real-time safe. Converting to and from
//! text, and saving and loading state, allocate and belong elsewhere.

mod param;
mod range;
mod set;
mod smoother;

pub use manguebyte_core::ParamId;
pub use param::{BoolParam, ChoiceParam, FloatParam, IntParam, Param, ParamFlags, ParamInfo};
pub use range::FloatRange;
pub use set::{LoadReport, ParamSet, ParamSetError, ParamState, ParseStateError};
pub use smoother::{Changed, ParamSmoother, Smoothing};
