mod cli;
mod fsutil;
mod git;
mod jsonutil;
mod lockfile;
mod manifest;
mod paths;
mod resolver;
mod schema;
mod store;
mod vendor;

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
