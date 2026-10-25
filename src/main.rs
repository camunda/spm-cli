mod cli;
mod fsutil;
mod git;
mod gitignore;
mod ids;
mod jsonfile;
mod jsonutil;
mod links;
mod lockfile;
mod manifest;
mod materialized;
mod owned;
mod pathid;
mod paths;
mod plugin;
mod policy;
mod remote;
mod resolver;
mod scan;
mod schema;
mod scope;
mod shell;
mod skillcheck;
mod store;
mod transitive;
mod vendor;
mod version;

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
