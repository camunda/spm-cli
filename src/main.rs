mod cli;
mod fsutil;
mod git;
mod gitignore;
mod jsonutil;
mod links;
mod lockfile;
mod manifest;
mod paths;
mod plugin;
mod policy;
mod resolver;
mod scan;
mod schema;
mod scope;
mod skillcheck;
mod store;
mod transitive;
mod vendor;

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
