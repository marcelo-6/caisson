use std::process::ExitCode;

fn main() -> ExitCode {
    match caisson::cli::run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
