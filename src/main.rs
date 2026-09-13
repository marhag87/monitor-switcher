//! The console binary. Everything it does lives in the library, which the tray
//! daemon shares; all that is left here is the subsystem it is compiled for —
//! this one has a console and prints to it.

use std::process::ExitCode;

fn main() -> ExitCode {
    monitor_switcher::app::main()
}
