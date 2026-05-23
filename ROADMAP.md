# Roadmap: Neo4j And Qdrant

This roadmap tracks storage/search work beyond the current SQLite/FTS5 runtime.

## Neo4j Graph Memory

Goal: add optional graph-backed memory for entities, relationships, decisions, files, and sessions.

- Define a graph model for projects, sessions, prompts, observations, files, concepts, decisions, and summaries.
- Add a Rust `GraphStore` trait with a no-op implementation and a Neo4j implementation behind a feature flag.
- Build idempotent migration jobs from SQLite rows to Neo4j nodes/relationships.
- Preserve SQLite as the source of truth during initial rollout.
- Add graph lookup APIs for related files, related decisions, concept neighborhoods, and session lineage.
- Add e2e tests that seed SQLite, run migration, and verify Neo4j relationship queries.

## Qdrant Vector Search

Goal: replace the old Chroma vector layer with a Qdrant-backed semantic index.

- Define embedding record IDs that remain stable across SQLite, Qdrant, and future graph references.
- Add a `VectorStore` trait with SQLite-only fallback and Qdrant implementation behind a feature flag.
- Add collection bootstrap, schema/version metadata, and health checks.
- Add batch backfill from existing observations, prompts, and summaries.
- Add incremental indexing hooks after new memory writes.
- Add hybrid retrieval that combines Qdrant semantic results with SQLite FTS5 filters.
- Add migration tooling for Chroma-to-Qdrant if an old Chroma directory is present.
- Add e2e tests with an optional Qdrant container or externally supplied Qdrant URL.

## Migration Principles

- Keep SQLite authoritative until graph/vector migrations are repeatable and observable.
- Make migrations resumable and idempotent.
- Store migration checkpoints under `CLAUDE_MEM_HOME`.
- Keep feature flags off by default until CI covers the integration path.
- Preserve the current worker HTTP and hook contracts while adding new capabilities.
