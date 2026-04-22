# Fuzz Testing

This directory contains fuzz tests for `gix-index` using [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html).

## Running Fuzz Tests

### Prerequisites
- Nightly Rust toolchain: `rustup install nightly`
- cargo-fuzz: `cargo install cargo-fuzz`

### Targets
- `index_file`: exercises index decoding, entry/path access, extension access, verification, and round-trip serialization.

### Running the target
```bash
cargo +nightly fuzz run index_file -- -max_total_time=60
```
