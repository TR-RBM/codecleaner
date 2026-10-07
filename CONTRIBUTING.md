# Contributing

This page describes how to build and test codecleanup and which rules its code and documents follow. The project is in the public domain under the [Unlicense](UNLICENSE); by submitting a change you agree to dedicate it to the public domain in the same way.

## Build

codecleanup is one Rust crate that builds the single binary `codecleanup`.

```sh
cargo build
cargo build --release     # target/release/codecleanup
```

You need the current stable Rust. Nightly features are not used. The dependencies are the ones in `Cargo.toml`; a change that adds a dependency says why it is needed.

The WebAssembly plugins under `plugins/` are a workspace of their own. Their compiled `.wasm` files are checked in under `languages/` and embedded into the binary, so building the tool needs no WebAssembly toolchain. You only need one to change a plugin:

```sh
rustup target add wasm32-unknown-unknown
plugins/build.sh          # builds the plugins and copies them to languages/
```

## The gate

A change is ready when these pass. The `ci` workflow runs the same on every push and pull request, with the tests on Linux and Windows:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

and, if you touched `plugins/`:

```sh
cd plugins
cargo fmt --check --all
cargo clippy --locked --release --target wasm32-unknown-unknown -- -D warnings
```

Every clippy warning is an error. The runner uses the newest stable toolchain, so a lint can fail there that an older local toolchain does not know yet.

## Tests

Tests are unit tests next to the code they check, and they are kept simple: a source text goes in, the expected result is compared.

| File | Covers |
|---|---|
| `src/rules.rs` | the TOML rule lexer, including windows of a few bytes |
| `src/plugins.rs` | each built-in language, TOML and WebAssembly |
| `src/process.rs` | replacing, grouping, existing refids, restore |
| `src/store.rs` | the Markdown docs and the index |

A change to a lexer or to the rewriting can destroy code silently, which a handful of cases does not rule out. For such a change, also run a round trip over real code and say with your change that you did and on what:

```sh
cp -r some-project copy
codecleanup --in=copy --docs=docs
codecleanup --in=copy --docs=docs --restore
diff -r some-project copy      # must print nothing
```

Between the two runs the cleaned copy should still build or parse like the original.

## Adding a language

Most languages need only rules:

1. Add `languages/<name>.toml`. The format is described in the README, and the existing files are examples.
2. List the file in `BUILTIN_RULES` in `src/plugins.rs`.
3. Add a test for it in `src/plugins.rs`, with the cases where the language is tricky: comment tokens inside strings, escapes, character literals.
4. Add a row to the language table in the README.

Write a WebAssembly plugin only when rules cannot express the syntax. Add a crate under `plugins/` that implements `Language` from the `sdk` crate, add it to `plugins/Cargo.toml` and `plugins/build.sh`, commit the built `.wasm`, and list it in `BUILTIN_WASM`.

When in doubt, a lexer leaves text alone. A comment that is not replaced is a small flaw; code that is replaced is data loss.

## Code rules

- **Rust only**, apart from the shell script `plugins/build.sh`, the packaging files under `packaging/` and `assets/make-demo.py`.
- **Plugins classify, the host does I/O.** A plugin never opens, reads or writes a file.
- **Files are streamed.** Nothing may hold a whole source file or a whole doc in memory.
- **The docs format and the index stay readable.** Docs and indexes written by an earlier version must still restore. A change that breaks this needs a migration and a note in the release.
- **Comments say why**, not what, and match the density of the code around them.
- **English** for code, comments and documents.

## Documents

- A change of behaviour says, in the text that comes with it, what it changes and why, what was run with which results, and what was not tested.
- A user-visible change comes with its section in the README.
- If the output of the tool changes, regenerate the demo: `cargo build --release && python3 assets/make-demo.py`.
- Plain sentences, short headings. Never describe something planned as if it existed.

## Commits and changes

- The subject is `area: short summary`, lower case, at most about 72 characters, no full stop: `rules: handle raw strings of any level`. Several areas are separated by commas.
- The body explains why, the cause, and how it was verified, with measured values rather than claims. What changed is in the diff.
- Keep one change to one subject.
- Never commit credentials or private keys.

## Releasing

See the Releasing section of the README.

## Security

Do not report a vulnerability in a public change or issue. See [SECURITY.md](SECURITY.md).
