//! Operating systems, processor architectures and plugin formats.
//!
//! Everything here is a plain value with no access to the machine it runs
//! on, apart from [`Os::host`]. That is what lets the layout of a macOS
//! bundle be computed, and tested, on Windows.

use crate::error::Error;

/// An operating system plugins are bundled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    /// Windows.
    Windows,
    /// Linux.
    Linux,
    /// macOS.
    Macos,
}

impl Os {
    /// All of them, in the order they are listed in messages.
    pub const ALL: [Self; 3] = [Self::Windows, Self::Linux, Self::Macos];

    /// The system this program was built for, or `None` on a system plugins
    /// are not bundled for.
    pub fn host() -> Option<Self> {
        if cfg!(target_os = "windows") {
            Some(Self::Windows)
        } else if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Some(Self::Macos)
        } else {
            None
        }
    }

    /// The name people use for it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Linux => "Linux",
            Self::Macos => "macOS",
        }
    }

    /// The extension, without the dot, of a dynamic library built by cargo
    /// for this system.
    pub fn library_extension(self) -> &'static str {
        match self {
            Self::Windows => "dll",
            Self::Linux => "so",
            Self::Macos => "dylib",
        }
    }
}

/// A processor architecture.
///
/// Only the ones whose folder name inside a VST 3 bundle is known from the
/// VST 3 documentation are listed. Others are refused by
/// [`Target::from_triple`] with an error, instead of guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Arch {
    /// 64-bit x86.
    X86_64,
    /// 64-bit ARM.
    Aarch64,
    /// 32-bit x86.
    X86,
}

impl Arch {
    /// The name Rust gives it.
    pub fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::X86 => "x86",
        }
    }
}

/// What a library was built for: a system and an architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Target {
    /// The operating system.
    pub os: Os,
    /// The processor architecture.
    pub arch: Arch,
}

impl Target {
    /// Reads a Rust target triple such as `x86_64-pc-windows-msvc`.
    ///
    /// ```
    /// use cargo_mangue::platform::{Arch, Os, Target};
    ///
    /// let target = Target::from_triple("aarch64-apple-darwin")?;
    /// assert_eq!(target.os, Os::Macos);
    /// assert_eq!(target.arch, Arch::Aarch64);
    /// // Android has "linux" in its triple and is not Linux.
    /// assert!(Target::from_triple("aarch64-linux-android").is_err());
    /// # Ok::<(), cargo_mangue::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Fails for a system other than Windows, Linux and macOS, and for an
    /// architecture other than x86_64, aarch64 and 32-bit x86.
    pub fn from_triple(triple: &str) -> Result<Self, Error> {
        let mut parts = triple.split('-');
        let arch = parts.next().unwrap_or_default();
        let rest: Vec<&str> = parts.collect();
        let has = |word: &str| rest.contains(&word);

        let os = if has("windows") {
            Os::Windows
        } else if has("darwin") {
            Os::Macos
        } else if has("linux") && !has("android") && !has("ohos") {
            Os::Linux
        } else {
            return Err(Error::usage(format!(
                "The target \"{triple}\" is for a system cargo-mangue does not bundle plugins \
                 for. Use a target for Windows, Linux or macOS, such as \
                 x86_64-pc-windows-msvc, x86_64-unknown-linux-gnu or aarch64-apple-darwin."
            )));
        };
        let arch = match arch {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            "i686" | "i586" => Arch::X86,
            _ => {
                return Err(Error::usage(format!(
                    "The target \"{triple}\" has a processor architecture cargo-mangue does \
                     not bundle for. Use an x86_64, aarch64 or i686 target."
                )));
            }
        };
        Ok(Self { os, arch })
    }
}

/// Finds the host triple in what `rustc -vV` prints.
///
/// ```
/// use cargo_mangue::platform::host_triple;
///
/// let printed = "rustc 1.85.0 (4d91de4e4 2025-02-17)\nhost: x86_64-unknown-linux-gnu\n";
/// assert_eq!(host_triple(printed), Some("x86_64-unknown-linux-gnu"));
/// assert_eq!(host_triple("rustc 1.85.0"), None);
/// ```
pub fn host_triple(rustc_version: &str) -> Option<&str> {
    rustc_version
        .lines()
        .find_map(|line| line.strip_prefix("host:"))
        .map(str::trim)
        .filter(|triple| !triple.is_empty())
}

/// A plugin format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// CLAP, the CLever Audio Plug-in format.
    Clap,
    /// Steinberg's VST 3.
    Vst3,
}

impl Format {
    /// Both formats, in the order they are bundled.
    pub const ALL: [Self; 2] = [Self::Clap, Self::Vst3];

    /// The extension of a plugin in this format, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Clap => "clap",
            Self::Vst3 => "vst3",
        }
    }

    /// The name people use for it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Clap => "CLAP",
            Self::Vst3 => "VST 3",
        }
    }

    /// Reads a format as written on the command line: `clap` or `vst3`, in
    /// any case.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "clap" => Some(Self::Clap),
            "vst3" => Some(Self::Vst3),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(triple: &str) -> (Os, Arch) {
        let target = Target::from_triple(triple).unwrap();
        (target.os, target.arch)
    }

    #[test]
    fn reads_the_triples_rust_ships_for_desktop_systems() {
        use Arch::{Aarch64, X86, X86_64};
        use Os::{Linux, Macos, Windows};

        assert_eq!(target("x86_64-pc-windows-msvc"), (Windows, X86_64));
        assert_eq!(target("x86_64-pc-windows-gnu"), (Windows, X86_64));
        assert_eq!(target("x86_64-pc-windows-gnullvm"), (Windows, X86_64));
        assert_eq!(target("aarch64-pc-windows-msvc"), (Windows, Aarch64));
        assert_eq!(target("i686-pc-windows-msvc"), (Windows, X86));
        assert_eq!(target("x86_64-unknown-linux-gnu"), (Linux, X86_64));
        assert_eq!(target("x86_64-unknown-linux-musl"), (Linux, X86_64));
        assert_eq!(target("aarch64-unknown-linux-gnu"), (Linux, Aarch64));
        assert_eq!(target("i686-unknown-linux-gnu"), (Linux, X86));
        assert_eq!(target("i586-unknown-linux-gnu"), (Linux, X86));
        assert_eq!(target("x86_64-apple-darwin"), (Macos, X86_64));
        assert_eq!(target("aarch64-apple-darwin"), (Macos, Aarch64));
    }

    #[test]
    fn refuses_other_systems_and_architectures_with_a_usage_error() {
        for triple in [
            "aarch64-linux-android",
            "aarch64-apple-ios",
            "aarch64-apple-ios-macabi",
            "wasm32-unknown-unknown",
            "x86_64-unknown-freebsd",
            "aarch64-unknown-linux-ohos",
            "armv7-unknown-linux-gnueabihf",
            "riscv64gc-unknown-linux-gnu",
            "arm64ec-pc-windows-msvc",
            "",
            "-",
            "windows",
            "x86_64",
        ] {
            let error = Target::from_triple(triple).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{triple}");
            assert!(error.message().contains(triple), "{error}");
        }
    }

    #[test]
    fn finds_the_host_among_the_lines_rustc_prints() {
        let printed = "rustc 1.99.0 (b940084d7 2026-09-28)\nbinary: rustc\n\
                       commit-hash: b940084d7\nhost: x86_64-pc-windows-msvc\nrelease: 1.99.0\n";
        assert_eq!(host_triple(printed), Some("x86_64-pc-windows-msvc"));
        assert_eq!(
            host_triple("host: aarch64-apple-darwin\r\n"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(host_triple(""), None);
        assert_eq!(host_triple("host:"), None);
        assert_eq!(host_triple("the host: is elsewhere"), None);
    }

    #[test]
    fn formats_are_read_in_any_case_and_nothing_else_is() {
        assert_eq!(Format::from_name("clap"), Some(Format::Clap));
        assert_eq!(Format::from_name("VST3"), Some(Format::Vst3));
        assert_eq!(Format::from_name("vst"), None);
        assert_eq!(Format::from_name(""), None);
        assert_eq!(Format::Clap.extension(), "clap");
        assert_eq!(Format::Vst3.extension(), "vst3");
    }

    #[test]
    fn this_machine_is_one_of_the_systems_or_none() {
        // The tests run on the three systems CI covers.
        if cfg!(any(
            target_os = "windows",
            target_os = "linux",
            target_os = "macos"
        )) {
            assert!(Os::host().is_some());
        }
    }
}
