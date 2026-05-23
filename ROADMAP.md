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

Status: initial optional self-hosted Qdrant support exists behind the worker `qdrant` feature.

Implemented:

- Observation point IDs are stable SQLite observation IDs.
- Qdrant is feature-gated and runtime-gated by `CLAUDE_MEM_QDRANT_*` env vars.
- Collection bootstrap is automatic.
- New observations are indexed opportunistically after memory writes.
- `/api/vector/qdrant/reindex` backfills recent/project-scoped observations.
- `/api/search?strategy=qdrant` searches Qdrant and falls back to SQLite.
- Unit/integration/e2e coverage uses a fake Qdrant server, plus optional `QDRANT_URL` smoke coverage.

Remaining:

- Add explicit schema/version metadata in collection payload or a sidecar marker.
- Extend indexing beyond observations to prompts and summaries.
- Add hybrid ranking that merges Qdrant scores with SQLite FTS5/BM25 results.
- Add migration tooling for Chroma-to-Qdrant if an old Chroma directory is present.
- Add containerized CI coverage for real Qdrant.

## Migration Principles

- Keep SQLite authoritative until graph/vector migrations are repeatable and observable.
- Make migrations resumable and idempotent.
- Store migration checkpoints under `CLAUDE_MEM_HOME`.
- Keep feature flags off by default until CI covers the integration path.
- Preserve the current worker HTTP and hook contracts while adding new capabilities.
