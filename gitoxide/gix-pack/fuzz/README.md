# Fuzz Testing

This directory contains fuzz tests for `gix-pack` using [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html).

## Running Fuzz Tests

### Prerequisites
- Nightly Rust toolchain: `rustup install nightly`
- cargo-fuzz: `cargo install cargo-fuzz`

### Targets
- `data_file`: exercises `.pack` parsing, checksum verification, entry lookup, and entry decoding.
- `index_file`: exercises `.idx` parsing, checksum verification, lookups, prefix lookups, and iteration.
- `multi_index_file`: exercises multi-pack-index parsing, checksum verification, lookups, prefix lookups, and iteration.

### Running a specific target
```bash
cargo +nightly fuzz run data_file -- -max_total_time=60
cargo +nightly fuzz run index_file -- -max_total_time=60
cargo +nightly fuzz run multi_index_file -- -max_total_time=60
```

### Running all targets
```bash
for target in data_file index_file multi_index_file; do
    cargo +nightly fuzz run "$target" -- -max_total_time=60
done
```
