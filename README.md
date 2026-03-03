# Hanzo Vector

High-performance vector search engine for the Hanzo AI platform. Based on [Qdrant](https://github.com/qdrant/qdrant).

## Quick Start

```bash
docker run -p 6333:6333 -p 6334:6334 ghcr.io/hanzoai/vector:latest
```

- REST API on port `6333`
- gRPC API on port `6334`

## Features

- Dense and sparse vector search with filtering and payloads
- Hybrid search combining semantic and keyword matching
- Vector quantization (scalar, product, binary) for memory efficiency
- Distributed deployment with sharding and replication
- HNSW indexing with SIMD acceleration
- Write-ahead logging and snapshot-based persistence
- GPU acceleration (NVIDIA and AMD)

## Configuration

Mount a custom config file:

```bash
docker run -p 6333:6333 \
  -v $(pwd)/config.yaml:/qdrant/config/production.yaml \
  -v $(pwd)/data:/qdrant/storage \
  ghcr.io/hanzoai/vector:latest
```

See `config/config.yaml` for all available options.

## Building from Source

```bash
cargo build --release --bin qdrant
```

## API

- **REST**: OpenAPI 3.0 schema in `openapi/`
- **gRPC**: Protocol definitions in `lib/api/src/grpc/proto/`

## Links

- [Hanzo AI](https://hanzo.ai)
- [GitHub](https://github.com/hanzoai/vector)
- [Qdrant Documentation](https://qdrant.tech/documentation/) (upstream docs)

## License

Apache License, Version 2.0. See [LICENSE](LICENSE).

---

*Based on [Qdrant](https://github.com/qdrant/qdrant) -- vector similarity search engine.*
