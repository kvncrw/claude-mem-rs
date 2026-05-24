# TS v12 vs Rust Boot Context Deltas

This fixture documents intentional boot-context differences between the source TS v12 `context-generator.cjs` output and the Rust port.

- Rust uses a compact markdown table for boot context rows; TS v12 uses line-oriented rows and bold detail blocks for fully expanded observations.
- Rust renders ASCII type IDs (`B`, `F`, `D`) for agent-safe table density; TS v12 renders mode emojis.
- Rust groups observations by file path inside each day; TS v12 day grouping is flat in non-pretty output.
- Rust includes session summary rows as `#S{id}` at the summary timestamp; TS v12 displays summaries as `S{id}` at the following summary's timestamp when available.
- Rust read-token estimates sum title, subtitle, narrative, and fact string lengths; TS v12 uses `JSON.stringify(facts)` and includes JSON overhead.
- Rust includes session discovery tokens in the boot-context work total; TS v12 work total is observation-only.
- Rust empty state says `No previous sessions found for this project yet.`; TS v12 says `No previous sessions found.`
