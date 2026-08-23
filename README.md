# cx

## Run

```sh
nix run github:nicolaschan/cx
```

Or with Docker:

```sh
docker run ghcr.io/nicolaschan/cx
```

Prebuilt binaries for Linux (x86_64, aarch64) and macOS (aarch64) are attached
to [releases](https://github.com/nicolaschan/cx/releases); tagging `v*`
publishes them.

## Develop

```sh
nix develop   # shell with the pinned toolchain (rust-toolchain.toml) + rust-analyzer
cargo test
```

The Rust toolchain and all dependencies come in through `flake.nix` via
[oxalica/rust-overlay](https://github.com/oxalica/rust-overlay). `nix build`
builds the binary and runs the tests; `nix build .#docker` builds the container
image, published multiarch by
[publish-nix-image](https://github.com/nicolaschan/publish-nix-image).
