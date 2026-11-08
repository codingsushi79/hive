mod cli;
mod config;
mod daemon;
mod git;
mod ipc;
mod model;
mod paths;
mod prompts;
mod review;
mod skills;
mod stream;
mod tui;

/// One timestamped line to the daemon log (the daemon's stderr).
pub fn log(message: String) {
    eprintln!(
        "{} {message}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
}

fn main() {
    if let Err(e) = cli::run() {
        eprintln!("hive: {e:#}");
        std::process::exit(1);
    }
}
