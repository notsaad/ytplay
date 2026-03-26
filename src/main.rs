mod app;

use std::process::ExitCode;

fn main() -> ExitCode {
    match app::run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::from(1)
        }
    }
}
