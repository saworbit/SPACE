# Fuzz targets

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the parsers that read bytes from a client or from disk ([#243](https://github.com/saworbit/SPACE/issues/243)). This crate isn't a member of the workspace, because libFuzzer needs nightly Rust and the workspace builds on stable.

| Target | What it reads |
|--------|---------------|
| `aws_chunked` | An `aws-chunked` PUT body, a few bytes at a time, with or without a decoded length (`protocol_s3::aws_chunked::decode`). A body that decodes must have the length it claims, and one that's refused must say why. |
| `delete_xml` | A DeleteObjects body (`parse_delete` in `crates/protocol-s3/src/handlers.rs`). |
| `complete_xml` | A CompleteMultipartUpload body (`parse_complete`, same file). |
| `checksum` | Digest header values (`checksum::Expected`), and a body digested in pieces, which must match the same body digested whole. |
| `journal_record` | A journal record's header, with the checksum skipped so every decoder sees arbitrary bytes. A record that decodes must encode to the same bytes after one round trip, and the torn tail walk (`format::journal_header_prefix`) must agree with the decoder about where the header ends. |
| `record_file` | A journal or data log whose bytes after the `FILE` record are arbitrary, opened as a store opens it (`record_file::open`, `NvramLog::open`). |
| `policy_yaml` | A policy file, as `spacectl create --policy-file` reads it. |

The two XML parsers are private; the `fuzzing` feature of `protocol-s3` exposes them as `protocol_s3::handlers::fuzzing`, for these targets only.

## Running

With the nightly the CI job uses (`NIGHTLY` in [`.github/workflows/fuzz.yml`](../.github/workflows/fuzz.yml)) on Linux or macOS:

```bash
rustup toolchain install nightly-2026-11-14 --profile minimal
cargo install cargo-fuzz@0.13.1 --locked
cargo +nightly-2026-11-14 fuzz list
cargo +nightly-2026-11-14 fuzz run journal_record fuzz/corpus/journal_record fuzz/seeds/journal_record -- -max_total_time=300
```

The first directory gets the new inputs the fuzzer finds (`fuzz/corpus/` isn't tracked); `fuzz/seeds/` holds the inputs every run starts from. A crash leaves its input in `fuzz/artifacts/<target>/`. Replay it with `cargo +nightly-2026-11-14 fuzz run <target> <file>`, then fix it with a regression test in the crate that owns the parser.

The seeds come from the encoders and the tests' own vectors. Regenerate them after an on-disk format change:

```bash
cargo run --manifest-path fuzz/Cargo.toml --example seeds
```

`Cargo.lock` here starts from the workspace's, so the targets build the crates at the versions the workspace ships.
