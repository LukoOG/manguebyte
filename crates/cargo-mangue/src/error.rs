use std::fmt;

/// Why the tool stopped, as the number the process exits with.
///
/// Scripts and CI jobs can tell a wrong command line from a build that
/// failed and from a plugin that a validator rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Failure {
    /// The command could not do its work: a file could not be written, a
    /// tool is missing, the workspace has no plugin. Exit code 1.
    Failed,
    /// The command line was wrong. Exit code 2.
    Usage,
    /// `cargo build` failed. Exit code 3.
    Build,
    /// A validator ran and reported a failed test, or gave no result.
    /// Exit code 4.
    Validation,
    /// No validator was found, so nothing was checked. Exit code 5.
    NoValidator,
}

impl Failure {
    /// The process exit code that stands for this failure.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Failed => 1,
            Self::Usage => 2,
            Self::Build => 3,
            Self::Validation => 4,
            Self::NoValidator => 5,
        }
    }
}

/// An error with the sentence to show and the exit code to end with.
///
/// The message is written for the person at the terminal: it says what went
/// wrong and what to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    failure: Failure,
    message: String,
}

impl Error {
    /// An error of the given kind.
    pub fn new(failure: Failure, message: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
        }
    }

    /// The command could not do its work.
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(Failure::Failed, message)
    }

    /// The command line was wrong.
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(Failure::Usage, message)
    }

    /// `cargo build` failed.
    pub fn build(message: impl Into<String>) -> Self {
        Self::new(Failure::Build, message)
    }

    /// What kind of failure this is.
    pub fn failure(&self) -> Failure {
        self.failure
    }

    /// The process exit code to end with.
    pub fn exit_code(&self) -> u8 {
        self.failure.exit_code()
    }

    /// The sentence to show.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_failure_has_its_own_exit_code_and_none_is_zero() {
        let all = [
            Failure::Failed,
            Failure::Usage,
            Failure::Build,
            Failure::Validation,
            Failure::NoValidator,
        ];
        let codes: Vec<u8> = all.iter().map(|failure| failure.exit_code()).collect();
        assert_eq!(codes, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn an_error_shows_its_message_and_carries_its_code() {
        let error = Error::usage("Unknown option --frmat.");
        assert_eq!(error.to_string(), "Unknown option --frmat.");
        assert_eq!(error.exit_code(), 2);
        assert_eq!(Error::build("x").failure(), Failure::Build);
        assert_eq!(Error::failed("x").exit_code(), 1);
    }
}
