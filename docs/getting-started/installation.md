# Installation

`spm` ships as a single self-contained binary. It needs the system `git` on your
`PATH` at runtime — it shells out to `git` rather than bundling libgit2.

## From npm (recommended)

The zero-setup path on every platform. It puts `spm` on your `PATH` with no
manual steps:

```bash
npm i -g @camunda8/spm
spm --help
```

`@camunda8/spm` is a thin launcher that pulls in the matching prebuilt binary for
your OS/CPU via an optional dependency (`@camunda8/spm-<os>-<cpu>`), so nothing is
compiled or downloaded outside npm.

Supported platforms: `darwin-x64`, `darwin-arm64`, `linux-x64`, `linux-arm64`,
`win32-x64`.

Update with:

```bash
npm i -g @camunda8/spm@latest
```

## From crates.io

Build and install from source via Cargo (needs a Rust toolchain). The crate is
`spm-cli`; the installed binary is `spm`:

```bash
cargo install spm-cli
```

## Prebuilt binary

The repo is **internal**, so release assets require authentication. Download with
the [GitHub CLI](https://cli.github.com/) — you must be signed in via `gh auth
login` and be a Camunda org member — then put the binary on your `PATH`:

```bash
# pick the asset for your platform (see list below); example: Apple Silicon macOS
gh release download --repo camunda/spm-cli \
  --pattern 'spm-aarch64-apple-darwin' --output spm
chmod +x spm && sudo mv spm /usr/local/bin/
```

`--repo camunda/spm-cli` with no tag grabs the latest release; add a tag such as
`v0.1.0` as the first positional argument to pin a specific version.

Available assets:

- `spm-x86_64-unknown-linux-gnu`
- `spm-aarch64-unknown-linux-gnu`
- `spm-x86_64-apple-darwin`
- `spm-aarch64-apple-darwin`
- `spm-x86_64-pc-windows-msvc.exe`

## From source

```bash
git clone https://github.com/camunda/spm-cli && cd spm-cli
make install                  # release build → /usr/local/bin/spm
make install PREFIX=~/.local  # or a custom prefix
# or: cargo install --path .
```

## Next steps

- [Quick Start](/getting-started/quick-start) — add your first skill in a few commands.
- [Why spm?](/guide/why-spm) — the problem it solves and how.
