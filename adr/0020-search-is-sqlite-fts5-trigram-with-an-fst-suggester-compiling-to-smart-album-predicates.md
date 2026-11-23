# ADR-0020 — Search is SQLite FTS5 (trigram) with an fst suggester, compiling to smart-album predicates

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Search](../capsule-docs/src/content/docs/design/search.md), [Organization — Smart-Album Definition Schema](../capsule-docs/src/content/docs/design/organization.md#smart-album-definition-schema)
- **Issues:** #987, #1276

## Context

Capsule has no search engine. On master `a3e580e7`:

- The Swift search screen loads the timeline once and filters it in memory
  (`capsule-swift/Modules/FeatureSearch/Sources/SearchViewModel.swift:6-8,86-87`). It does not call
  `SearchPort`.
- `SearchPort` (`capsule-swift/Modules/CapsulePorts/Sources/IntelligencePorts.swift:135-166`) has
  only a mock. The mock scans at most 20,000 assets per query
  (`capsule-swift/Modules/CapsuleMock/Sources/MockSearchPort.swift`), and its own comment says a
  real local index replaces it.
- The catalog has no full-text table (`capsule-library/src/db/schema.rs`). It has `asset_tags`
  with an index on `tag`, and the `caption`, `latitude` and `longitude` projections (schema v5).
- The smart-album predicate grammar is specified (`design/organization.md`), but only its
  `tags_user` `contains` term is implemented (`capsule-core/src/sidecar/smart_album.rs`).
- `semantic_search` exists over `sqlite-vec` and runs only against the fixture runner
  (`capsule-core/src/ml/orchestrator.rs:282`).
- The README advertised "search by date, place, camera, lens, rating and label". Lens appears in no
  code, and import never fills `camera_id` (`capsule-library/src/lifecycle/import.rs:1057`).

The search must work offline, because the server holds only ciphertext. It must run identically on
iOS, Android, desktop and later the web, and stay inside a phone's memory at a million assets.

### Engines considered

- **Tantivy** (a Lucene-style engine in Rust). It has excellent ranking and fuzzy queries. But it
  is a second storage engine beside the catalog, with its own segment files, merge threads and
  crash semantics, kept in step with SQLite by hand. Its index is built per token, so CJK text
  needs a segmenter per language. Its binary and memory footprint are large for a phone and for
  wasm. Photo search is mostly structured filtering over a small vocabulary, which needs none of its
  scoring.
- **SQLite FTS5 with `unicode61` or `porter`.** It is word-based. It misses infix matches
  ("ach" in "beach", "York" inside "NewYork2019") and every language written without spaces.
- **SQLite `LIKE '%…%'` scans.** These are correct and need no index, but are O(n) per keystroke.
  That is what the mock does, and why it caps the scan.
- **An in-memory trie or a sorted `Vec` for suggestions.** It is simple, but costs heap that grows
  with the vocabulary on every launch. An `fst` map is immutable, compact and memory-mappable, so it
  costs page cache the OS can evict, not heap.
- **The `fst` crate's own `Levenshtein` automaton.** Its documentation calls it experimental, and
  its construction can use tens of MB before a state limit fails it. A precomputed parametric
  Levenshtein DFA (the Schulz–Mihov construction, as Tantivy's `levenshtein_automata` implements
  it) is built in time linear in the query, counts a transposition as one edit, and cannot blow up.
- **A bespoke query language.** It was rejected in favour of the operators users already know from
  Gmail, Discord and Google.
- **Keeping search separate from smart albums.** That would make two grammars and two evaluators,
  and "save as smart album" would become a lossy translation.

## Decision

Search is local-only:

- SQLite FTS5 with the `trigram` tokenizer, in the catalog database, over a **vocabulary** of
  distinct names and over captions, never one row per asset per name;
- plus a memory-mapped `fst` suggester answering fuzzy prefix queries through a parametric
  Levenshtein DFA.

Both are derived state, rebuilt from sidecars, never synced. The query grammar (`@`, `#`, `in:`,
`from:`, `camera:`, `before:`/`after:`/`on:`, `near:`, `has:`, `is:`, `-`, `"…"`, `OR`, parentheses)
is parsed in `capsule-core`. The chip row and the text field are two views of one AST, which
compiles into the smart-album predicate tree. That tree is extended with `text`, `camera`, `lens`,
spatial and `media_subtype` terms so the compile is total.

`@` binds to people and labels and `#` to labels only, because Capsule labels are untyped and, until
people clusters ship, a person is named by a region label.

Semantic search is a ranking mode merged by reciprocal rank fusion, never a predicate term.

## Consequences

- One SQLite engine (the bundled 3.46.0, with FTS5 compiled in) serves the catalog, the vectors and
  search on every platform. No platform's system SQLite is involved.
- FTS is a prefilter. The core's own folding decides membership, so smart albums evaluate
  identically even if SQLite's folding tables change. This costs a verification pass over
  candidates.
- Trigram covers CJK only for fragments of three or more characters. A one- or two-character
  fragment, which is most Chinese and Japanese words, cannot use the index. It is served by an
  `instr` scan over the name and caption tables, capped by rows examined and by time, and a
  capped result says it may be incomplete. The suggester does not fill the gap: it matches only a
  prefix of a vocabulary key, never a caption.
- A saved search freezes every local resolution (people, places, fuzzy matches) into the predicate.
  Renaming a label or updating the place dataset does not silently change a saved album.
- Saving a search drops its semantic part, and the UI says so.
- Camera and lens search requires import to fill `camera_id` and the new `capture_settings`.
- Qualifier keywords are English-only. The chips are localized.
- The web client gets search only with in-browser decryption, via the same core compiled to wasm.
