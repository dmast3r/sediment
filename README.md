# Sediment

Named for how LSM trees compact data into ever-denser layers: Sediment.

## Requirements

- Rust 1.85 or newer, installed with [rustup](https://rustup.rs/)

The repository includes `rust-toolchain.toml`, which tells rustup to use Rust
1.85.0 and install Clippy for the project.

## Check the project

```sh
cargo check
cargo clippy --all-targets --all-features -- -D warnings
```