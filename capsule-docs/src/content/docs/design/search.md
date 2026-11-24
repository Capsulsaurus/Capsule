---
title: Search
description: The local search index, the query grammar and its chips, typo-tolerant suggestions, and how search compiles to smart-album predicates
audit: none
---

Search runs **on the device, over the device's own catalog**. The server holds only ciphertext and
cannot evaluate a query, so it has no search surface ([](/design/api-surfaces/)). This page owns the
query grammar, the search index and the suggester. The predicate tree a query compiles to is owned
by [](/design/organization/#smart-album-definition-schema); place resolution by [](/design/places/);
embeddings by [](/design/ai/).

The load-bearing rule: **a query, a chip row and a smart album are one value.** The text grammar
and the chips are two renderings of one AST, and the AST compiles into the same predicate a smart
album stores, so "save this search as a smart album" is a copy, not a translation.

## Where It Lives

| Surface                                                   | Module                                 | Status                                                                        |
| --------------------------------------------------------- | -------------------------------------- | ----------------------------------------------------------------------------- |
| Grammar: parser, AST, printer, compiler to `Predicate`    | `capsule-core::search`                 | Planned                                                                       |
| Predicate evaluation                                      | `capsule-core::sidecar::smart_album`   | Partial: the `tags_user` `contains` term only                                 |
| Search index, suggester, query execution, recent searches | `capsule-library::search`              | Planned                                                                       |
| Semantic ranking                                          | `capsule-core::ml` (`semantic_search`) | Implemented against the fixture runner only ([](/design/ai/#semantic-search)) |

The grammar lives in the stateless core so the CLI, the FFI and the wasm build parse identically.
The index lives in `capsule-library`, beside the catalog it indexes. Apps reach both through the
FFI as four verbs, matching the Swift `SearchPort` (`search.query`, `search.suggest`,
`search.recent`, `search.clear_recent`). The CLI is the reference client: every verb, plus an
`explain` verb that prints the AST, its canonical text and the compiled predicate, is reachable
with `--json`.

**Today** no Rust search exists. The Swift search screen filters the timeline in memory
(`FeatureSearch/Sources/SearchViewModel.swift`), and `SearchPort` has only a mock that scans at
most 20,000 assets (`CapsuleMock/Sources/MockSearchPort.swift`). Android has no search surface.

## The Index

The index is **derived state**: rebuildable from the sidecars, the album and stack records and the
place-name cache, never synced, never backed up and never sent anywhere. It has two parts.

### Full-Text: FTS5 Trigram

SQLite FTS5 tables with the `trigram` tokenizer (`case_sensitive 0`, `remove_diacritics 1`) live in
the catalog database ([](/design/filesystem/client/#desktop-library-layout)). The SQLite build is
the one `capsule-library` bundles (3.46.0, FTS5 enabled), so every platform runs the same engine and
none depends on its OS's SQLite.

- **Two tables, not one row per asset.** A _vocabulary_ table holds each distinct searchable name
  once (a label, a person, an album name, a place name, a camera or lens) with its kind and the id
  it resolves to. A _caption_ table holds captions, the only free text that is per asset. A name
  is never copied into one row per asset that carries it, so the index grows with the number of
  distinct names and captions, not with the library.
- **Why trigram.** It matches any substring of three or more characters, including inside words and
  in scripts written without spaces (Chinese, Japanese, Thai), with no word segmenter and no
  per-language stemmer.
- **Short fragments are scanned, within a cap.** A trigram `MATCH` cannot find a fragment of one or
  two characters, and most Chinese and Japanese words are that short. Such a fragment is matched by
  a scan instead: `instr` over the core's folding (registered as an SQL function) of each row of the
  vocabulary table, then of the caption table. The scan stops after **20,000 rows examined or
  50 ms**, whichever comes first, enforced by SQLite's progress handler. A scan the cap stops returns
  what it found, and the result header says more may match. Its hits are candidates like any FTS
  hit. The suggester does not cover this case: it matches only a prefix of a vocabulary key, never
  a word inside a key or a caption.
- **Always parameterised.** A query string is built from parsed tokens, each one emitted as an FTS5
  string with `"` doubled, and bound as a parameter. Raw user input is never spliced into a
  `MATCH` expression or into SQL.
- **A prefilter, not the judge.** FTS5's folding is SQLite's. Smart-album membership must be
  identical on every device ([](/design/organization/#smart-album-definition-schema)), so an FTS
  hit is a candidate that the core's own `text` term evaluation confirms. A candidate the core
  rejects is dropped.

Structured axes (dates, rating, cull, media kind, album, location) are not in FTS at all. They are
B-tree indexes on the catalog's existing projections.

### Suggestions: fst

A memory-mapped `fst` map (an immutable finite-state transducer) holds every vocabulary key,
case-folded and stripped of diacritics, mapped to its vocabulary id. It answers **fuzzy prefix**
lookups with a Levenshtein automaton intersected with the map, which is how `@ana` finds `Anna`
and `bech` finds `beach`.

- **Edit budget.** 0 edits for a fragment of 1–3 characters, 1 for 4–7, 2 for 8 or more. A
  transposition counts as one edit. The automaton is built from a precomputed parametric DFA,
  whose construction is linear in the fragment and cannot blow up; a fragment longer than 64
  characters gets no fuzzy expansion.
- **Ranking.** Candidates are ordered by edit distance, then by kind (people, labels, places,
  albums, cameras), then by how many visible assets carry them, then by key bytes. The order is
  total, so the same library gives the same suggestions everywhere.
- **One pass per edit distance.** The map streams matches in key order, not rank order, so a
  lookup never truncates the stream. It runs the automaton for distance 0, then 1, then 2, up to
  the fragment's budget, and skips keys an earlier pass found. Each pass feeds a bounded selection
  of the best 256 under the order above. The lookup stops after the first pass that leaves 256
  candidates, since no candidate at a greater distance can outrank them. A lookup visits at most
  65,536 keys across its passes; when that cap stops a pass, the suggestions are what the
  selection holds and are marked incomplete.
- **Updates.** The map is immutable. New and removed keys go to a small in-memory delta, and the
  catalog keeps a per-key count; a key whose count is zero is filtered out. The map is rebuilt in
  the background when the delta passes 4,096 keys or the device is idle, written to a temporary
  file and renamed into place, then remapped.
- **Never rewritten in place.** A mapped file is only ever replaced by rename, so a live mapping
  never sees a truncated file. On open the file's header (magic, format version, key count,
  checksum) is validated before mapping. A missing, short or corrupt file is rebuilt from the
  vocabulary table, and suggestions fall back to FTS until it is.

### Budgets

The scale benchmark ([](/development/performance/)) measures these on the parity phone. They are
unmeasured targets until it runs.

| Measure                                                   | 1,000,000 assets | 5,000,000 assets |
| --------------------------------------------------------- | ---------------- | ---------------- |
| Search index on disk (FTS tables and the `fst` file)      | < 64 MiB         | < 320 MiB        |
| Heap held by search, beyond mapped pages the OS may evict | < 8 MiB          | < 16 MiB         |
| One suggestion lookup, per keystroke (p95, warm)          | < 16 ms          | < 16 ms          |
| First page of results (p95, warm; cold within 2×)         | < 200 ms         | < 200 ms         |
| Full index rebuild from the catalog                       | < 60 s           | < 5 min          |

The vocabulary design is what makes the disk budget reachable: names are stored once, so only
captions grow with the asset count.

### Freshness

Search index rows change **in the same catalog transaction** that projects a sidecar, album record,
stack record or place name into the catalog, so a search never sees a catalog row its index lacks.
A catalog rebuild ([](/design/filesystem/client/#local-index-staleness)) rebuilds the index with it.

### What Search Sees

- **Hidden and trashed assets are out of scope** for the default search, exactly as for every
  default view ([](/design/organization/#hidden-assets)). Search inside the Hidden view and inside
  Trash is scoped to that view, and the Hidden view's search runs only behind its fresh local-auth
  gate (SR1, [](/design/local-gallery/#security-requirements)).
- **Suggestions never leak a hidden asset.** The vocabulary counts each key's visible and hidden
  carriers separately. Default-scope suggestions draw only on keys with a visible carrier, so a
  label or place used only on hidden assets is never suggested outside the gate.
- **Remote albums** contribute only their breadcrumbs until promoted ([](/design/federation/)).
- **The web client** holds no keys in v1, so it has no search. With in-browser decryption it runs
  the same `capsule-core::search` grammar in wasm over an index it builds in the tab, and the
  server still learns nothing.

## Indexed Axes

Each axis reads the **effective** value: the user's register if written, else the as-imported field
([](/design/metadata/#as-imported-and-user-values)).

| Axis                     | Source                                                                                                   | Index                                                       |
| ------------------------ | -------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| Labels, at every level   | `tags_user`, live region labels, stack and album records ([](/design/organization/#labels))              | vocabulary + `asset_tags`                                   |
| AI tags                  | `tags_ai`, current generation only ([](/design/ai/#ai-output-containment))                               | vocabulary                                                  |
| People                   | named people ([](/design/people/)); before people clusters ship, region labels are how a person is named | vocabulary                                                  |
| Place names              | the place-name cache ([](/design/places/#place-names))                                                   | vocabulary                                                  |
| Captions                 | `caption_lww`                                                                                            | caption FTS                                                 |
| Recognised text          | the text-recognition output of the current model ([](/design/ai/#text-recognition))                      | text FTS                                                    |
| Album                    | container album and its name                                                                             | vocabulary + `album_id`                                     |
| Camera and lens          | `camera_id.model`, `capture_settings` ([](/design/metadata/#capture-settings))                           | vocabulary                                                  |
| Dates                    | effective capture time, import time                                                                      | B-tree                                                      |
| Media kind, content type | `content_type`, Live Photo stacks                                                                        | B-tree                                                      |
| Rating, cull, favorite   | `rating_lww`, `cull`                                                                                     | B-tree                                                      |
| Location                 | effective `gps`                                                                                          | the location index ([](/design/places/#the-location-index)) |

Camera and lens become searchable only once the sidecar carries them: import does not fill
`camera_id` today (`capsule-library/src/lifecycle/import.rs:1057`), and `capture_settings` is new.

## Query Grammar

The grammar follows the operator conventions users already know, rather than inventing new ones:
Gmail's `from:`, `in:`, `has:`, `is:`, `before:`/`after:`, `-` and `OR`
([Gmail search operators](https://support.google.com/mail/answer/7190)); Discord's `@` mentions,
`has:` and `during:`, completed from a picker as you type
([Discord search](https://support.discord.com/hc/en-us/articles/115000468588)); and Google's
quoted phrases, `-` and `before:`/`after:` with no space after the colon
([Google search operators](https://support.google.com/websearch/answer/2466433)).

```text
query     = expr?
expr      = conj
conj      = disj { [ "AND" ] disj }          ; adjacency is AND
disj      = unary { "OR" unary }             ; OR binds tighter than AND
unary     = [ "-" ] atom                     ; "-" negates one atom
atom      = "(" expr ")" | qualified | mention | hashtag | phrase | word
qualified = key ":" value                    ; no space around ":"
mention   = "@" ( phrase | word )
hashtag   = "#" ( phrase | word )
value     = phrase | word | comparison
comparison= ( ">=" | "<=" | ">" | "<" | "=" ) word
phrase    = '"' { char | '\"' } '"'
```

`OR` and `AND` are operators only in upper case, as in Gmail. `beach sunset OR sunrise` is `beach`
AND (`sunset` OR `sunrise`). Qualifier keys are ASCII and case-insensitive. They are accepted in
English only: the chips are fully localized ([](#chips-and-text)), so a user who never learns the
syntax never needs it.

| Syntax                                                                                                           | Meaning                                                                                             | Compiles to                                                                            |
| ---------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `word`, `"a phrase"`                                                                                             | free text ([](#free-text))                                                                          | see below                                                                              |
| `@name`                                                                                                          | a person or a label named _name_ ([](#what--binds-to))                                              | `Any` of the person and label terms that resolve                                       |
| `#name`, `tag:name`, `label:name`                                                                                | a label at any level                                                                                | `tags_user contains`                                                                   |
| `person:name`                                                                                                    | a named person only                                                                                 | person term ([](/design/organization/#smart-album-definition-schema))                  |
| `in:name`, `album:name`                                                                                          | an album                                                                                            | `album_id contains` (name resolved to id)                                              |
| `from:name`                                                                                                      | the camera or device it came from                                                                   | `Any(camera matches, device_id contains)`                                              |
| `camera:name`, `lens:name`                                                                                       | camera make or model; lens make or model                                                            | `camera matches`, `lens matches`                                                       |
| `before:D`, `after:D`, `on:D` (`during:D`)                                                                       | before the period _D_ starts; after it ends; within it                                              | `capture_time` range                                                                   |
| `near:place`                                                                                                     | near a named place ([](/design/places/#place-search))                                               | `gps near (point, radius)`                                                             |
| `has:location`, `has:caption`, `has:label`, `has:video`                                                          | has a location; a caption; any label; a moving part (a video or a Live Photo)                       | `gps exists`, `caption exists`, …                                                      |
| `is:favorite`, `is:pick`, `is:reject`, `is:raw`, `is:photo`, `is:video`, `is:live`, `is:screenshot`, `is:hidden` | rating 5 (the favourite rating); cull flags; a RAW `content_type`; media kind; a screenshot; hidden | `rating eq 5`, `cull is pick`, `content_type in raw`, `media_subtype eq screenshot`, … |
| `rating:>=4`, `iso:>3200`                                                                                        | numeric comparisons                                                                                 | numeric terms                                                                          |
| `-x`                                                                                                             | not _x_                                                                                             | `Not`                                                                                  |

- **Dates.** _D_ is ISO 8601: `2024`, `2024-06` or `2024-06-01`; `2024/06/01` is accepted too.
  Each names a **period**, so `after:2024` starts on 2025-01-01 and `before:2024-06` ends on
  2024-05-31. A date compares the capture's **local wall-clock** time, the date the person who took
  it saw. Slash dates other than year-first (`01/02/2024`) are ambiguous across regions and are
  read as free text, with a hint offering the ISO form.
- **Favourite** is the top rating, `5`, the value an exporter's favourite maps to
  (`capsule-library/src/import/enrichment.rs:67`). There is no separate flag.
- **`is:hidden`** is accepted only in the Hidden view's search. Anywhere else it is a diagnostic
  that offers to open the Hidden view; it never runs a search that could confirm a hidden asset
  exists.
- **Repeats are AND.** `#beach #sunset` needs both, as in Gmail. `in:a in:b` matches nothing,
  because an asset has one container album.

### What `@` Binds To

**`@` binds to people and labels; `#` binds to labels only.**

Discord, Slack and most chat tools use `@` to mention a name, and users reach for it for a tag as
readily as for a person. In Capsule the two are not separable by type: a label "carries no hierarchy and no type"
([](/design/organization/#labels)), and until people clusters ship, a person is named by labelling a
face region. A sigil that searched only one kind would make the user remember which kind they
stored a name as. So `@` is the **mention** sigil: it searches every named thing a person would
"mention", people first, then labels. `#` is the strict form for the user who means a label and
nothing else, and `person:` the strict form for a person.

A typed `@word` resolves when the query compiles. An exact person name and an exact label both
match; the term becomes `Any` of the two, and the chip shows both. With no exact match the
suggester's best candidate is offered and never applied silently.

### Free Text

A bare word or phrase matches, in the live search, any vocabulary name containing it (labels,
people, places, albums, cameras, lenses) any caption containing it and any recognised text containing it, after folding. Names match
by trigram substring, or by the capped scan for a one- or two-character word
([](#full-text-fts5-trigram)); a word with no substring hit expands through the suggester's fuzzy match,
and the result header says so ("Showing results for _beach_").

### Diagnostics, Not Errors

The parser is **total**: every input yields an AST plus diagnostics, and a search never fails on
syntax. An unclosed quote closes at the end; an unknown key (`foo:bar`) is free text with a
diagnostic; an unmatched parenthesis is dropped. Input is capped at 1 KiB, the AST at 64 terms and
depth 8 (the predicate's own limits), and anything past a cap is dropped with a diagnostic. No
operator takes a regular expression, a wildcard or unbounded input.

## Chips and Text

The chips UI and the text field are **two views of one AST**:

- Typing a qualifier, a mention or a hashtag and accepting a suggestion turns it into a chip.
  Pickers (dates, people, albums, places) create chips directly.
- Editing a chip edits its AST node. The text field shows the AST's **canonical text**: lower-case
  keys, values quoted when they contain a space or an operator character, one space between terms.
- `parse(print(ast)) == ast` for every AST, and the chip row is a function of the AST. Both are
  property-tested.

## Compiling to a Predicate

A query compiles to the predicate tree of [](/design/organization/#smart-album-definition-schema),
in two steps.

1. **Resolve.** Every reference to local, device-specific state is replaced by what it resolved
   to: a mention by its person and label terms, an album name by its id, `near:Paris` by a point
   and radius, a fuzzy word by the name it matched, a place-name match by its geometry.
2. **Lower.** The resolved AST maps one-to-one onto `All`, `Any`, `Not` and `Term`.

The live search runs the lowered predicate, so a saved search returns what the live search showed.
A saved predicate **never refers to** the vocabulary, the suggester, the place-name cache or the
embedding index. Those are per-device and versioned, and smart-album membership must be identical
on every device, so everything they contributed is frozen into the predicate at save time and shown
as chips.

### Ranking

- **Without free text**, results are a set, ordered by the sort key (default: effective capture
  time, newest first).
- **With free text**, results are ordered by match class (exact name, name prefix, substring,
  caption, fuzzy), then by the sort key.
- **With semantic search on**, see [](#semantic-search).

Matched entities (people, places, albums, labels) are listed above the assets as top hits.

## Semantic Search

Semantic search is a **ranking mode**, not a predicate term. The free text of the query (every word
and phrase not inside a qualifier) is embedded with the semantic-search embedder and matched against
the vector index ([](/design/ai/#semantic-search)). Structured terms filter both result lists
alike. The two rankings merge by reciprocal rank fusion with _k_ = 60, ties broken by asset id, and
every semantic-only hit carries its matched scope, so the UI can say why a photo with no "dog" label
matched "dog".

- **No network.** The embedder runs on the device and the index is local. A device without the
  model's weights runs lexical search only and says so.
- **Not savable.** Embedding similarity is not bit-exact across platform partitions and moves with
  every model generation, so it cannot meet the smart-album identity rule. Saving a semantic search
  saves its structured and lexical part, and the UI states that. Zero-shot AI tags, which carry
  their model slot, are the savable form of "what is in the photo".

**Is search semantic and fully modern today? No.** v1 search is lexical, structured and
typo-tolerant, which already exceeds a plain substring filter. Semantic ranking is wired against a
fixture runner only; real ranking needs the first ONNX Runtime runner ([](/design/ai/#semantic-search)).
Once that lands, search is hybrid with no network dependency.

## Recent Searches

Recent searches are kept on the device only, never synced and never backed up: a search history
records what someone went looking for, which is more sensitive than the photos it found. At most 20
are kept, Hidden-view searches are never recorded, and clearing removes them from the catalog
immediately.

## Known Gaps

- No Rust search exists; the Swift screen filters in memory and Android has no search.
- Camera and lens are not indexed until import fills `camera_id` and `capture_settings`.
- People are searchable only as region labels until people clusters ship ([](/design/people/)).
- Semantic ranking waits on the first real model runner.
- Recognised text is not indexed; text recognition does not exist yet.
- The budgets are unmeasured targets.

## Validation

- **Grammar (unit, property).** `parse(print(ast)) == ast`; the parser is total over arbitrary
  bytes (fuzzed); caps and diagnostics fire at their limits; precedence fixtures (`a b OR c`,
  `-(a OR b)`).
- **Compile (unit).** Each qualifier lowers to its documented term; a saved predicate contains no
  reference to local state; a query and its saved smart album return the same set on a fixture
  library.
- **Index (smoke).** The FTS and `fst` contents after incremental edits equal a fresh rebuild; a
  truncated or corrupt `fst` file is rebuilt, never mapped; an FTS `MATCH` built from hostile input
  (`"`, `*`, `NEAR(`, `^`) matches it literally.
- **Short fragments (unit).** A two-character Chinese word inside a caption and inside a name is
  found by the scan; a scan over more rows than its cap stops at the cap and marks the result.
- **Suggestion order (unit).** A distance-0 key that sorts after 256 other matching keys still
  ranks first; a lookup past the visit cap is marked incomplete.
- **Scope (unit).** No default-scope query or suggestion returns or reveals a hidden or trashed
  asset; `is:hidden` outside the Hidden view runs nothing.
- **Determinism (unit).** Suggestions and result order are identical for the same catalog on every
  target, wasm included.
