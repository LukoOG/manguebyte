//! Which web pages may connect: the `Origin` a browser sends with the
//! handshake, checked against an allow-list.
//!
//! The list is [`DEFAULT_ORIGINS`] and whatever the environment variable
//! [`ORIGINS_VARIABLE`] (comma-separated) and the file [`origins_file`]
//! (one origin a line, `#` starting a comment) add. An entry is an origin
//! as a browser writes it, `scheme://host` or `scheme://host:port`, and
//! `:*` stands for any port: `http://localhost:*` allows a development
//! server on any port of the machine. Matching ignores case, and a default
//! port (80 for `http`, 443 for `https`) is the same as none.

use std::path::PathBuf;

/// The origins allowed without any configuration: Aratu, and pages served
/// from this machine.
pub const DEFAULT_ORIGINS: [&str; 5] = [
    "https://aratu.live",
    "http://localhost:*",
    "http://127.0.0.1:*",
    "https://localhost:*",
    "https://127.0.0.1:*",
];

/// The environment variable that adds origins, separated by commas.
pub const ORIGINS_VARIABLE: &str = "ARATU_BRIDGE_ORIGINS";

/// The file that adds origins, one a line: `aratu-bridge/origins.txt` in
/// the user's configuration folder (`%APPDATA%` on Windows,
/// `~/Library/Application Support` on macOS, `$XDG_CONFIG_HOME` or
/// `~/.config` elsewhere). `None` if there is no such folder.
pub fn origins_file() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }?;
    Some(base.join("aratu-bridge").join("origins.txt"))
}

/// An origin split into its parts, in lower case.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parts {
    scheme: String,
    host: String,
    /// `None` for any port, else the port, the default one filled in.
    port: Option<u16>,
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

/// Splits `origin`; `wildcard` allows `:*`.
fn parse(origin: &str, wildcard: bool) -> Option<Parts> {
    let origin = origin.trim().to_ascii_lowercase();
    let (scheme, rest) = origin.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() || rest.contains('/') || rest.contains('@') {
        return None;
    }
    // A bracketed IPv6 host keeps its colons.
    let (host, port) = match rest.rfind(':') {
        Some(at) if !rest[at..].contains(']') => (&rest[..at], Some(&rest[at + 1..])),
        _ => (rest, None),
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    let port = match port {
        None => Some(default_port(scheme)?),
        Some("*") if wildcard => None,
        Some(digits) => Some(digits.parse().ok()?),
    };
    Some(Parts {
        scheme: scheme.to_owned(),
        host: host.to_owned(),
        port,
    })
}

/// The origins pages may connect from.
#[derive(Debug, Clone, Default)]
pub struct AllowList {
    entries: Vec<Parts>,
}

impl AllowList {
    /// A list of `entries`; those that are not origins are left out and
    /// returned.
    pub fn new<'a>(entries: impl IntoIterator<Item = &'a str>) -> (Self, Vec<String>) {
        let mut list = Self::default();
        let mut bad = Vec::new();
        for entry in entries {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            match parse(entry, true) {
                Some(parts) => list.entries.push(parts),
                None => bad.push(entry.to_owned()),
            }
        }
        (list, bad)
    }

    /// [`DEFAULT_ORIGINS`] and what the environment and the file add.
    pub fn from_environment() -> (Self, Vec<String>) {
        let mut entries: Vec<String> = DEFAULT_ORIGINS.iter().map(|&s| s.to_owned()).collect();
        if let Ok(value) = std::env::var(ORIGINS_VARIABLE) {
            entries.extend(value.split(',').map(str::to_owned));
        }
        if let Some(text) = origins_file().and_then(|path| std::fs::read_to_string(path).ok()) {
            entries.extend(
                text.lines()
                    .map(|line| line.split('#').next().unwrap_or("").to_owned()),
            );
        }
        Self::new(entries.iter().map(String::as_str))
    }

    /// Whether a page from `origin` may connect. `null`, which a browser
    /// sends for a file or a sandboxed frame, never may.
    pub fn allows(&self, origin: &str) -> bool {
        let Some(origin) = parse(origin, false) else {
            return false;
        };
        self.entries.iter().any(|entry| {
            entry.scheme == origin.scheme
                && entry.host == origin.host
                && entry.port.is_none_or(|port| Some(port) == origin.port)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> AllowList {
        AllowList::new(DEFAULT_ORIGINS).0
    }

    #[test]
    fn aratu_and_local_pages_are_allowed() {
        let list = defaults();
        for origin in [
            "https://aratu.live",
            "https://ARATU.live",
            "https://aratu.live:443",
            "http://localhost:5173",
            "http://localhost",
            "http://127.0.0.1:8080",
            "https://localhost:3000",
        ] {
            assert!(list.allows(origin), "{origin}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        let list = defaults();
        for origin in [
            "http://aratu.live",
            "https://aratu.live:8443",
            "https://evil.example",
            "https://aratu.live.evil.example",
            "https://evilaratu.live",
            "http://localhost.evil.example:80",
            "null",
            "",
            "file://",
            "https://aratu.live/path",
            "https://user@aratu.live",
            "http://localhost:*",
            "http://localhost:99999",
        ] {
            assert!(!list.allows(origin), "{origin:?}");
        }
    }

    #[test]
    fn entries_can_be_added_and_bad_ones_are_reported() {
        let (list, bad) = AllowList::new([
            "https://studio.example:8443",
            "http://[::1]:*",
            "not an origin",
            "ftp://",
            "",
        ]);
        assert_eq!(bad, ["not an origin", "ftp://"]);
        assert!(list.allows("https://studio.example:8443"));
        assert!(!list.allows("https://studio.example"));
        assert!(list.allows("http://[::1]:3000"));
    }
}
