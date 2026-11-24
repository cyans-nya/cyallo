use clap::Parser;

#[derive(Parser)]
#[command(version, about = "Cyallo and CAIE pseudocode language server")]
struct Cli {
    /// Communicate using the Language Server Protocol over stdin/stdout (default).
    #[arg(long)]
    stdio: bool,
}

fn main() -> std::process::ExitCode {
    Cli::parse();
    match cya_ls::server::run_stdio() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cya-ls: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
