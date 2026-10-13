use std::fmt;

use manguebyte_params::{LoadReport, ParamSet, ParamState, ParseStateError};

use crate::Plugin;

const HEADER: &str = "manguebyte-plugin";
const VERSION: u32 = 1;

/// Why saved state could not be restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// The text is not plugin state at all.
    NotPluginState,
    /// The state was written by a newer, incompatible version of the format.
    UnsupportedVersion(u32),
    /// The state belongs to a different plugin. Holds that plugin's id.
    WrongPlugin(String),
    /// The parameter values inside the state could not be read.
    Params(ParseStateError),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPluginState => f.write_str("not plugin state: header line missing"),
            Self::UnsupportedVersion(version) => {
                write!(f, "plugin state format version {version} is not supported")
            }
            Self::WrongPlugin(id) => write!(f, "this state belongs to the plugin \"{id}\""),
            Self::Params(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Params(error) => Some(error),
            _ => None,
        }
    }
}

/// Captures a plugin's state as text.
///
/// The text names the plugin and holds every parameter's real value by
/// identifier. It is safe to call from any thread while audio is running:
/// the values are read from atomics and nothing is locked.
///
/// ```text
/// manguebyte-plugin 1 org.example.gain
/// manguebyte-params 1
/// gain=0.5
/// ```
pub fn save_state<P: Plugin>(params: &P::Params) -> String {
    format!("{HEADER} {VERSION} {}\n{}", P::INFO.id, params.save())
}

/// Restores state captured by [`save_state`].
///
/// State from another version of the same plugin loads as far as it can; the
/// report says which parameters were missing or unknown. State from a
/// different plugin is refused, and nothing is changed. Safe to call from
/// any thread while audio is running.
pub fn load_state<P: Plugin>(params: &P::Params, text: &str) -> Result<LoadReport, StateError> {
    let (header, rest) = text.split_once('\n').unwrap_or((text, ""));
    let mut fields = header.split_whitespace();
    if fields.next() != Some(HEADER) {
        return Err(StateError::NotPluginState);
    }
    let version: u32 = fields
        .next()
        .and_then(|field| field.parse().ok())
        .ok_or(StateError::NotPluginState)?;
    if version != VERSION {
        return Err(StateError::UnsupportedVersion(version));
    }
    let id = fields.next().ok_or(StateError::NotPluginState)?;
    if id != P::INFO.id {
        return Err(StateError::WrongPlugin(id.to_owned()));
    }
    let state: ParamState = rest.parse().map_err(StateError::Params)?;
    Ok(params.load(&state))
}
