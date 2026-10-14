//! The `cargo-mangue` program. Everything it does is in the library of the
//! same name.

use std::process::ExitCode;

fn main() -> ExitCode {
    cargo_mangue::main(std::env::args_os())
}
