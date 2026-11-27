# RTerm tests

`unit/` mirrors the application modules in `src/`. These are Rust unit
tests, loaded from the owning module with `#[cfg(test)]` and `#[path = ...]`.
Keeping them as child modules allows testing private implementation details
without making those details public in the production API.

Add tests and test-only fixtures to the matching file in `tests/unit/`.
When adding a new test module, place its gated declaration at the end of
the owning source file. Small test-only fields or hooks may remain in
production types when necessary; do not put test cases in `src/`.

```sh
cargo test --locked --all-features --bins
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --package rterm -- --check
```

Platform-specific tests keep their existing platform gates. A passing
Linux run does not validate the Windows or macOS implementations.
Manual benchmarks remain ignored by default.
