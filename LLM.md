# LLM.md - Hanzo Vector

## Overview
High-performance vector search engine for the Hanzo AI platform.

**Upstream**: [Qdrant](https://github.com/qdrant/qdrant) (Apache-2.0).

## Tech Stack
- **Language**: Rust

## Build & Run
```bash
cargo build
cargo test
```

## Structure
```
vector/
  Cargo.lock
  Cargo.toml
  Dockerfile
  LICENSE
  LLM.md
  README.md
  clippy.toml
  config/
  docs/
  lib/
  openapi/
  pkg/
  rustfmt.toml
  shell.nix
  src/
```

## Key Files
- `README.md` -- Project documentation
- `Cargo.toml` -- Rust crate config
- `Dockerfile` -- Container build
