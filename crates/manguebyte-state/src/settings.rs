//! A settings file: keys and values in a text file in the user's
//! configuration folder, for what an application remembers between runs
//! (window size, recent files, the audio device) as opposed to a document.
//!
//! | System | Folder |
//! |---|---|
//! | Windows | `%APPDATA%\<app>` (the roaming application data) |
//! | macOS | `$HOME/Library/Application Support/<app>` |
//! | Linux and other Unix systems | `$XDG_CONFIG_HOME/<app>`, or `$HOME/.config/<app>` when that is unset or not absolute |
//! | Anything else, such as WebAssembly | none: [`Settings::config_dir`] is `None` |
//!
//! A file is a versioned header and one `key=value` line per setting,
//! sorted by key:
//!
//! ```text
//! manguebyte-settings 1
//! audio.buffer=256
//! recent=C:\\Users\\ana\\kit.mbstate
//! window.width=900
//! ```
//!
//! In values, a backslash, a line feed and a carriage return are written
//! `\\`, `\n` and `\r`. Reading skips lines it cannot read, and reads a file
//! of a newer version as far as it can, as `DeviceSetup` and parameter
//! state do.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::value::is_valid_name;

const HEADER: &str = "manguebyte-settings";
const VERSION: u32 = 1;
const EXTENSION: &str = "settings";

/// The systems whose configuration folders differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Windows,
    Macos,
    Unix,
    Other,
}

impl Os {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(unix) {
            Self::Unix
        } else {
            Self::Other
        }
    }
}

/// The folder settings of every application live in, on `os`, reading the
/// environment through `env`.
fn config_base(os: Os, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    // Absolute by the rules of `os`, whatever system this runs on, so that
    // every system's rules are tested everywhere.
    let absolute = |name: &str| {
        env(name).map(PathBuf::from).filter(|path| match os {
            Os::Windows => !path.as_os_str().is_empty(),
            _ => path.to_string_lossy().starts_with('/'),
        })
    };
    match os {
        Os::Windows => absolute("APPDATA"),
        Os::Macos => absolute("HOME").map(|home| home.join("Library").join("Application Support")),
        Os::Unix => absolute("XDG_CONFIG_HOME")
            .or_else(|| absolute("HOME").map(|home| home.join(".config"))),
        Os::Other => None,
    }
}

/// Whether `name` can be a folder or file name on every system.
fn is_valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.trim() == name
        && !name.ends_with('.')
        && name.chars().all(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

fn invalid(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what)
}

/// Keys and values an application keeps between runs. See the
/// [module](self) for where the file goes and what it looks like.
///
/// ```no_run
/// use manguebyte_state::Settings;
///
/// let mut settings = Settings::open("Mangue Sampler", "app")?;
/// let width = settings.get_f64("window.width").unwrap_or(900.0);
/// settings.set("window.width", width + 10.0);
/// settings.set("recent", r"C:\Users\ana\kit.mbstate");
/// settings.save()?;
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    path: Option<PathBuf>,
    values: BTreeMap<String, String>,
}

impl Settings {
    /// Empty settings that belong to no file.
    pub fn new() -> Self {
        Self::default()
    }

    /// The folder for the settings of the application `app` on this
    /// system, as the [module](self) lists. Nothing is created.
    ///
    /// `None` on a system with no such folder, when the environment does
    /// not say where it is, or when `app` cannot be a folder name.
    pub fn config_dir(app: &str) -> Option<PathBuf> {
        Self::config_dir_with(app, &|name| std::env::var_os(name))
    }

    fn config_dir_with(app: &str, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
        if !is_valid_file_name(app) {
            return None;
        }
        config_base(Os::current(), env).map(|base| base.join(app))
    }

    /// The settings `name` of the application `app`: the file
    /// `<name>.settings` in [`config_dir`](Self::config_dir)`(app)`,
    /// read if it exists and empty otherwise. [`save`](Self::save) writes
    /// it back, creating the folder.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] if `app` or `name` cannot be a file
    /// name, [`io::ErrorKind::NotFound`] if the system has no configuration
    /// folder, and the errors of [`load`](Self::load).
    pub fn open(app: &str, name: &str) -> io::Result<Self> {
        if !is_valid_file_name(name) {
            return Err(invalid(format!("{name:?} cannot be a file name")));
        }
        if !is_valid_file_name(app) {
            return Err(invalid(format!("{app:?} cannot be a folder name")));
        }
        let folder = Self::config_dir(app).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "this system has no configuration folder",
            )
        })?;
        Self::load(folder.join(format!("{name}.{EXTENSION}")))
    }

    /// The settings in the file at `path`, or empty settings for it if the
    /// file does not exist.
    ///
    /// # Errors
    ///
    /// An error reading the file, and [`io::ErrorKind::InvalidData`] if it
    /// is not a settings file.
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let mut settings = match fs::read_to_string(&path) {
            Ok(text) => text
                .parse::<Self>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::new(),
            Err(error) => return Err(error),
        };
        settings.path = Some(path);
        Ok(settings)
    }

    /// Writes the settings to their file, creating its folder if needed.
    /// The file is written beside and then renamed over the old one, so a
    /// crash midway leaves the old settings, not half of the new.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] for settings that belong to no file,
    /// and any error writing it.
    pub fn save(&self) -> io::Result<()> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| invalid(String::from("these settings belong to no file")))?;
        self.save_to(path)
    }

    /// Writes the settings to the file at `path`, as [`save`](Self::save)
    /// does, without making it theirs.
    ///
    /// # Errors
    ///
    /// Any error writing it.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(folder) = path
            .parent()
            .filter(|folder| !folder.as_os_str().is_empty())
        {
            fs::create_dir_all(folder)?;
        }
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        fs::write(&temporary, self.to_string())?;
        fs::rename(&temporary, path).inspect_err(|_| {
            let _ = fs::remove_file(&temporary);
        })
    }

    /// The file the settings belong to.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The value of `key`, as text.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// The value of `key` as a number, if it reads as one.
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key)?.trim().parse().ok()
    }

    /// The value of `key` as a whole number, if it reads as one.
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key)?.trim().parse().ok()
    }

    /// The value of `key` as a switch: `true` or `false`.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key)?.trim().parse().ok()
    }

    /// Sets `key` to `value`, written as its [`Display`](fmt::Display)
    /// text, which reads back exactly for numbers and switches.
    ///
    /// # Panics
    ///
    /// Panics if `key` is not a valid name (see
    /// [`is_valid_name`]).
    pub fn set(&mut self, key: &str, value: impl fmt::Display) {
        assert!(is_valid_name(key), "invalid settings key {key:?}");
        self.values.insert(key.to_owned(), value.to_string());
    }

    /// Removes `key`, returning its value.
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.values.remove(key)
    }

    /// Every key and value, sorted by key.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// How many settings there are.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl fmt::Display for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{HEADER} {VERSION}")?;
        for (key, value) in &self.values {
            write!(f, "{key}=")?;
            for c in value.chars() {
                match c {
                    '\\' => f.write_str("\\\\")?,
                    '\n' => f.write_str("\\n")?,
                    '\r' => f.write_str("\\r")?,
                    c => write!(f, "{c}")?,
                }
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

/// Why text is not a settings file.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseSettingsError {
    /// The text does not start with the `manguebyte-settings` header.
    MissingHeader,
}

impl fmt::Display for ParseSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader => f.write_str("not a settings file: header line missing"),
        }
    }
}

impl std::error::Error for ParseSettingsError {}

impl FromStr for Settings {
    type Err = ParseSettingsError;

    /// Reads settings written by [`Display`](fmt::Display), skipping lines
    /// it cannot read, whatever version wrote them.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut lines = text.lines();
        lines
            .next()
            .and_then(|line| line.trim().strip_prefix(HEADER))
            .and_then(|rest| rest.trim().parse::<u32>().ok())
            .ok_or(ParseSettingsError::MissingHeader)?;
        let mut settings = Self::new();
        for line in lines {
            let Some((key, raw)) = line.split_once('=') else {
                continue;
            };
            if !is_valid_name(key) {
                continue;
            }
            let mut value = String::with_capacity(raw.len());
            let mut chars = raw.chars();
            let mut readable = true;
            while let Some(c) = chars.next() {
                match c {
                    '\\' => match chars.next() {
                        Some('\\') => value.push('\\'),
                        Some('n') => value.push('\n'),
                        Some('r') => value.push('\r'),
                        _ => readable = false,
                    },
                    c => value.push(c),
                }
            }
            if readable {
                settings.values.insert(key.to_owned(), value);
            }
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn each_system_has_its_folder() {
        let windows = [("APPDATA", r"C:\Users\ana\AppData\Roaming"), ("HOME", "/x")];
        assert_eq!(
            config_base(Os::Windows, &env(&windows)),
            Some(PathBuf::from(r"C:\Users\ana\AppData\Roaming"))
        );
        assert_eq!(config_base(Os::Windows, &env(&[("HOME", "/x")])), None);

        let home = [("HOME", "/Users/ana")];
        assert_eq!(
            config_base(Os::Macos, &env(&home)),
            Some(
                Path::new("/Users/ana")
                    .join("Library")
                    .join("Application Support")
            )
        );

        assert_eq!(
            config_base(Os::Unix, &env(&[("HOME", "/home/ana")])),
            Some(Path::new("/home/ana").join(".config"))
        );
        // `XDG_CONFIG_HOME` wins, but only when absolute.
        let xdg = [("HOME", "/home/ana"), ("XDG_CONFIG_HOME", "/cfg")];
        assert_eq!(
            config_base(Os::Unix, &env(&xdg)),
            Some(PathBuf::from("/cfg"))
        );
        let relative = [("HOME", "/home/ana"), ("XDG_CONFIG_HOME", "cfg")];
        assert_eq!(
            config_base(Os::Unix, &env(&relative)),
            Some(Path::new("/home/ana").join(".config"))
        );
        assert_eq!(config_base(Os::Macos, &env(&[("HOME", "relative")])), None);
        assert_eq!(config_base(Os::Unix, &env(&[])), None);
        assert_eq!(config_base(Os::Other, &env(&home)), None);
    }

    #[test]
    fn application_names_must_be_folder_names() {
        for good in ["Mangue Sampler", "app", "org.example.x"] {
            assert!(is_valid_file_name(good), "{good:?}");
        }
        for bad in ["", ".", "..", "a/b", r"a\b", "a:b", " a", "a.", "tab\t"] {
            assert!(!is_valid_file_name(bad), "{bad:?}");
            assert_eq!(Settings::config_dir_with(bad, &env(&[])), None);
            assert_eq!(
                Settings::open(bad, "app").unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn settings_round_trip_through_text() {
        let mut settings = Settings::new();
        settings.set("window.width", 900.5);
        settings.set("count", -3);
        settings.set("on", true);
        settings.set("path", r"C:\Users\ana\new folder");
        settings.set("lines", "one\ntwo\r\nthree = 3");
        settings.set("empty", "");
        let text = settings.to_string();
        assert!(text.starts_with("manguebyte-settings 1\ncount=-3\n"));
        let read: Settings = text.parse().unwrap();
        assert_eq!(read, settings);
        assert_eq!(read.get_f64("window.width"), Some(900.5));
        assert_eq!(read.get_i64("count"), Some(-3));
        assert_eq!(read.get_bool("on"), Some(true));
        assert_eq!(read.get("lines"), Some("one\ntwo\r\nthree = 3"));
        assert_eq!(read.get_f64("path"), None);
        assert_eq!(read.len(), 6);
    }

    #[test]
    fn reading_is_tolerant() {
        let text =
            "manguebyte-settings 7\nok=1\nno equals sign\nbad key=2\nbad=\\q\n=3\nlater=x=y\n";
        let read: Settings = text.parse().unwrap();
        assert_eq!(
            read.iter().collect::<Vec<_>>(),
            [("later", "x=y"), ("ok", "1")]
        );
        assert_eq!(
            "ok=1".parse::<Settings>(),
            Err(ParseSettingsError::MissingHeader)
        );
    }

    #[test]
    fn settings_save_and_load_from_a_file() {
        let folder =
            std::env::temp_dir().join(format!("manguebyte-state-settings-{}", std::process::id()));
        let path = folder.join("nested").join("app.settings");
        let _ = fs::remove_dir_all(&folder);

        let mut settings = Settings::load(&path).unwrap();
        assert!(settings.is_empty(), "a missing file is empty settings");
        assert_eq!(settings.path(), Some(path.as_path()));
        settings.set("theme", "dark");
        settings.save().unwrap();
        settings.set("theme", "light");
        settings.save().unwrap();

        let again = Settings::load(&path).unwrap();
        assert_eq!(again.get("theme"), Some("light"));
        assert!(!path.with_extension("settings.tmp").exists());

        fs::write(&path, "not settings").unwrap();
        assert_eq!(
            Settings::load(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            Settings::new().save().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        fs::remove_dir_all(&folder).unwrap();
    }
}
