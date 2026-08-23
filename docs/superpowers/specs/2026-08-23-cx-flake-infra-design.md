# cx: flake + CI infrastructure design (2026-08-23)

Approved design for scaffolding the `cx` repo. The crate itself is a
placeholder; the deliverable is the infrastructure around it.

## Crate

Minimal Rust binary crate `cx` (edition 2024) that prints `cx <version>`,
with one unit test so CI's test step is exercised. `rust-toolchain.toml`
(stable channel) is the single source of truth for the toolchain, used by
both rustup and the flake.

## Flake

Canonical oxalica setup:

- Inputs: `nixpkgs` (nixos-unstable), `rust-overlay` (oxalica, follows
  nixpkgs), `flake-utils`.
- Toolchain via `rust-bin.fromRustupToolchainFile ./rust-toolchain.toml`,
  so nix and rustup agree by construction.
- `packages.default`: `makeRustPlatform` + `buildRustPackage` (tests run in
  the check phase; deps vendored from `Cargo.lock`). Chosen over
  crane/naersk for being the boring canonical path with no extra inputs;
  the trade-off is coarser build caching, irrelevant at this size.
- `packages.docker`: `dockerTools.buildLayeredImage` wrapping the binary as
  entrypoint — the shape expected by
  [publish-nix-image](https://github.com/nicolaschan/publish-nix-image)'s
  default `.#docker` attribute.
- `apps.default` so `nix run github:nicolaschan/cx` works.
- `devShells.default` with the toolchain + rust-analyzer.

## CI

- `ci.yml`: on push/PR, install Nix (Determinate Systems installer), run
  `nix flake check` and `nix build`. On tag `v*`, build on
  x86_64-linux / aarch64-linux / aarch64-darwin runners and attach the
  binaries to a GitHub Release.
- `docker.yml`: on push to master, call the
  `nicolaschan/publish-nix-image` reusable workflow, which builds
  `.#docker` on native amd64 and arm64 runners and pushes a combined
  multiarch manifest to `ghcr.io/nicolaschan/cx`.

## Repo

Default branch `master`; remote `git@github.com:nicolaschan/cx.git`
(public, so `nix run github:nicolaschan/cx` resolves).
