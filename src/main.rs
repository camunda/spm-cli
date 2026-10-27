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

/// The configuration every property test (`proptest!`) runs with. The seed is
/// fixed, so each run explores the same inputs (a property never fails on one
/// run and passes on the next); set `PROPTEST_RNG_SEED` to explore others
/// locally. Failures are not persisted, so no `proptest-regressions/` file
/// lands in the tree: a failure prints its shrunk input, which becomes a
/// focused regression test.
#[cfg(test)]
fn proptest_config(cases: u32) -> proptest::test_runner::Config {
    use proptest::test_runner::{Config, RngSeed};
    let base = Config::default();
    let rng_seed = match base.rng_seed {
        RngSeed::Random => RngSeed::Fixed(0x5eed_5b31),
        chosen => chosen,
    };
    Config {
        cases,
        failure_persistence: None,
        rng_seed,
        ..base
    }
}

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
