//! Builds [CLAP](https://cleveraudio.org) plugins from a
//! [`Plugin`](manguebyte_plugin::Plugin).
//!
//! CLAP is an open plugin standard whose model matches Manguebyte's own:
//! notes, per-note expression, parameter automation and modulation all
//! arrive as events with a frame position. Nothing has to be approximated on
//! the way in, so a plugin behaves in a host exactly as it does in the test
//! [`Harness`](manguebyte_plugin::Harness).
//!
//! # Making a plugin
//!
//! In a crate of type `cdylib`, implement `Plugin` and export it:
//!
//! ```ignore
//! manguebyte_clap::export_clap!(MyPlugin);
//! ```
//!
//! Build it, rename the resulting library to `MyPlugin.clap` and put it in a
//! folder your host scans.
//!
//! # What the wrapper provides
//!
//! | CLAP feature | Behaviour |
//! |---|---|
//! | Audio ports | Main input and output from the plugin's layout, plus a sidechain input if it has one. In-place processing is allowed when input and output match |
//! | Note port | For instruments and note effects, in the CLAP and MIDI dialects |
//! | Events in | Note on, off and choke; note expression; parameter value and modulation; MIDI. All keep their frame |
//! | Parameters | Listing, values, text conversion and flush. Continuous parameters are exposed as positions in `0..=1`, stepped ones as whole steps |
//! | Events out | A parameter changed by anything other than the host is reported to the host |
//! | State | Saved and restored as text; another plugin's state is refused |
//! | Latency | Reported from the plugin each time it is activated |
//!
//! A panic inside the plugin is caught at the boundary and reported to the
//! host as an error, rather than unwinding into it or aborting it.

mod wrapper;

pub use clap_sys;
pub use wrapper::plugin_entry;

/// Exports a plugin as the CLAP entry point of this library.
///
/// Use it once, in a crate whose `crate-type` is `cdylib`.
#[macro_export]
macro_rules! export_clap {
    ($plugin:ty) => {
        /// The entry point a CLAP host looks for.
        #[allow(non_upper_case_globals)]
        #[unsafe(no_mangle)]
        pub static clap_entry: $crate::clap_sys::entry::clap_plugin_entry =
            $crate::plugin_entry::<$plugin>();
    };
}
