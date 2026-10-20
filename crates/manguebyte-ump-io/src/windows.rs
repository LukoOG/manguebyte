//! Windows MIDI Services: whether it is installed, and why nothing here
//! sends through it.
//!
//! Windows MIDI Services is Windows' MIDI 2.0 stack. Its service,
//! `midisrv.exe`, ships with Windows 11 and also carries the older MIDI
//! 1.0 APIs (WinMM, which `midir` uses, and `Windows.Devices.Midi`): with
//! the service running, a MIDI 1.0 port opened through WinMM reaches a
//! MIDI 2.0 device through the service, which translates the bytes into
//! Universal MIDI Packets.
//!
//! Sending Universal MIDI Packets unchanged needs the service's own API,
//! the Windows MIDI Services App SDK (`Microsoft.Windows.Devices.Midi2`),
//! which this crate cannot reach:
//!
//! - It is a WinRT component that is not part of Windows' own metadata.
//!   The `windows` crate (0.62 in this workspace's lock file) is generated
//!   from the Windows SDK's metadata and has `Windows.Devices.Midi`, whose
//!   messages are MIDI 1.0 bytes, but no `Midi2` namespace.
//! - Its metadata (`Microsoft.Windows.Devices.Midi2.winmd`) comes in the
//!   SDK's NuGet package, and a projection would be generated from it by
//!   `windows-bindgen`, which is not among the dependencies either.
//!   Declaring its COM interfaces by hand would need the interface
//!   identifiers and method order that only that metadata gives.
//! - Its runtime is installed separately from Windows (the "App SDK
//!   Runtime"), and a desktop program must start it with the SDK's
//!   initializer, which the SDK ships as a C++ header, before activating
//!   any of its classes.
//!
//! So on Windows every port is reached through WinMM, as MIDI 1.0 bytes,
//! and [`midi_services`] only says what is installed, for the message an
//! application shows.

use std::path::PathBuf;

/// What of Windows MIDI Services is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MidiServices {
    /// The service, `%SystemRoot%\System32\midisrv.exe`.
    pub service: bool,
    /// The App SDK runtime's folder, `%ProgramFiles%\Windows MIDI
    /// Services`.
    pub sdk_runtime: bool,
}

fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
}

fn program_files() -> PathBuf {
    // A 32-bit process sees the 32-bit folder as `ProgramFiles`.
    std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
}

/// Looks for Windows MIDI Services on this machine. Reads the file system
/// only; the service is not started.
pub fn midi_services() -> MidiServices {
    let root = system_root();
    // A 32-bit process is redirected from System32; Sysnative is the real
    // one for it.
    let service = ["System32", "Sysnative"]
        .iter()
        .any(|folder| root.join(folder).join("midisrv.exe").is_file());
    MidiServices {
        service,
        sdk_runtime: program_files().join("Windows MIDI Services").is_dir(),
    }
}

/// Why this crate sends no Universal MIDI Packets on a machine with
/// `services`, in a sentence for an application to show.
pub fn unsupported_reason(services: MidiServices) -> String {
    let found = match (services.service, services.sdk_runtime) {
        (false, _) => {
            return String::from(
                "Windows MIDI Services is not installed (no midisrv.exe), so ports are \
                 reached through WinMM, as MIDI 1.0",
            );
        }
        (true, false) => "its service is installed, its App SDK runtime is not",
        (true, true) => "its service and its App SDK runtime are installed",
    };
    format!(
        "Windows MIDI Services: {found}, but its API (Microsoft.Windows.Devices.Midi2) is a \
         WinRT component that the windows crate does not project, so ports are reached \
         through WinMM, as MIDI 1.0, which the service translates for MIDI 2.0 devices"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reason_says_what_is_installed() {
        let none = unsupported_reason(MidiServices::default());
        assert!(none.contains("not installed"), "{none}");
        let service = unsupported_reason(MidiServices {
            service: true,
            sdk_runtime: false,
        });
        assert!(service.contains("App SDK runtime is not"), "{service}");
        assert!(service.contains("WinMM"), "{service}");
        let both = unsupported_reason(MidiServices {
            service: true,
            sdk_runtime: true,
        });
        assert!(both.contains("are installed"), "{both}");
    }

    #[test]
    fn looking_for_the_service_touches_only_the_file_system() {
        let found = midi_services();
        println!("{found:?}: {}", unsupported_reason(found));
    }
}
