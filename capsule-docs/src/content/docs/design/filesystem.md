---
title: Filesystem
description: How filesystem is structured in Capsule
---

Content in Capsule is organized on both server and client filesystems. Design priorities differ between the two, but both follow the same core principles.

## Core Principles

These principles apply universally to both client and server.

**Recovery-First**: The filesystem must be reconstructible from partial corruption. No database is required to interpret critical data — sidecar files are the canonical metadata store; the database is a rebuildable query cache.

**Deterministic**: File placement is algorithmic. The same media capture timestamp always produces the same path, enabling verification and repair.

**Self-Describing**: Each media file is paired with a CBOR sidecar containing all user-editable and stable metadata. Files are independently interpretable without a running database.

**Atomic Writes**: Use temp-file + rename throughout. Direct overwrites risk corruption on power loss.

**Postel's Law**: Liberal in what we accept — unknown sidecar fields preserved, missing optional fields tolerated. Strict in what we create — every required field must be present and valid before committing.

---

## On-Server Filesystem

The server stores authoritative copies of media files and sidecars for each library. Cache artifacts (thumbnails, transcodes, `.meta.cbor`) are ephemeral and regenerated on demand.

### Ownership Model

Content is partitioned at the root by `owner_id`. An owner is the billing and namespace entity (a person, organization, or team). The bijective mapping between `owner_id` and the set of authorized `user_id`s is managed entirely in PostgreSQL — the filesystem has no knowledge of individual user identities. Access control is enforced at the API layer, not the filesystem layer. `owner_id`, `album_id`, and `user_id` are all independent namespaces; no two of these ever share a value.

### Server Layout

```text
{data_root}/                             # Configured at server startup (e.g., /var/capsule)
├── {owner_id}/                        # One directory per owner; UUID-named, no prefix
│   └── {album_id}/                  # One directory per library under that owner
│       ├── media/
│       │   └── {YYYY}/
│       │       └── {YYYY-MM}/
│       │           ├── {uuid}.{ext}     # Media file (read-only after commit)
│       │           └── {uuid}.cbor      # Sidecar metadata (mutable)
│       ├── cache/
│       │   ├── meta/
│       │   │   └── {uuid[0:2]}/{uuid[2:4]}/{uuid}.meta.cbor      # Ephemeral full metadata
│       │   ├── thumbnails/
│       │   │   └── {xs|s|m|l|xl|o}/
│       │   │       └── {uuid[0:2]}/{uuid[2:4]}/
│       │   │           ├── {uuid}.jxl
│       │   │           └── {uuid}.webp
│       │   └── transcodes/
│       │       ├── h264/
│       │       │   └── {uuid[0:2]}/{uuid[2:4]}/{uuid}.mp4        # H.264/MP4 (ephemeral)
│       │       └── live/
│       │           └── {uuid[0:2]}/{uuid[2:4]}/{uuid}.mov        # Live Photo video (ephemeral)
│       └── trash/
│           └── {uuid}.{ext}             # Soft-deleted media (30-day quarantine)
└── .server/
    ├── version.cbor                     # Server filesystem schema version
    └── config.cbor                      # Server-wide configuration
```

**Path derivation**: Every file path on the server is fully deterministic from `(owner_id, album_id, asset_id, artifact_type)`. No scanning or database lookup is needed to compute a path.

**`{data_root}`**: Absolute path configured at server startup. The entire tree must be on a single filesystem to guarantee atomic renames within any library.

**`owner_id`**: Assigned at account creation. Never reused or changed. Because UUIDs are lowercase hex + hyphens and `.server` starts with a dot, there is no naming collision between owner directories and the server config directory.

**Transcode type directories** (`h264/` and `live/`): Each transcode type is a separate subdirectory instead of a filename suffix. The path for any transcode is unambiguously derived from the asset UUID and the type — no suffix convention to remember, no risk of name collision within a directory.

### Differences from Client Layout

| Concern            | Client                                          | Server                                       |
| ------------------ | ----------------------------------------------- | -------------------------------------------- |
| Root partitioning  | N/A (single library per root)                   | `{owner_id}/{album_id}/`                     |
| Index              | `index/library.sqlite` (file-based)             | PostgreSQL (rebuildable from sidecars)       |
| Lock               | `.library/lock` (per-library process lock file) | Process-managed; no lock file                |
| Config / version   | `.library/{version,config}.cbor`                | `.server/{version,config}.cbor` at data root |
| Migrations         | `.library/migrations/`                          | Server-managed; not on the filesystem        |
| Cache artifacts    | `index/{meta,thumbnails,transcodes}/`           | `cache/{meta,thumbnails,transcodes}/`        |
| Transcode subtypes | `index/transcodes/{h264,live}/{shard}/`         | `cache/transcodes/{h264,live}/{shard}/`      |
| Trash              | `.library/trash/`                               | `{album_id}/trash/`                          |

### Server Config Schema (`.server/config.cbor`)

```text
schema_version    u8     REQUIRED. Config schema version. Current: 1.
```

### Server Index (PostgreSQL)

The server uses PostgreSQL as its authoritative index. The core tables (`assets`, `asset_stacks`, `stack_members`, `asset_tags`) mirror the client's SQLite schema (same names and semantics). The server schema is a superset with additional server-only columns and tables.

**Server-only columns on `assets`**: `owner_id`, `chromahash`, `dominant_color`, `content_type`, `is_favorite`, `upload_user_id`, `uploaded`.

**Server-only tables**: `asset_stacks.metadata` (JSONB — stack-type-specific structured data), `owners`, `users`, `owner_members`, `albums`, `album_shares`, `people`, `faces`, `smart_tags`, `asset_smart_tags`, `share_links`, `memories`, `passkeys`.

Schema version is stored in a `_schema_meta` table row instead of `PRAGMA user_version`.

The server database is rebuildable from sidecars by the same scan-and-ingest logic as the client. On startup, if the database is empty and sidecar files are present, trigger a full rebuild. Stack relationships are reconstructed from `stack_hint` fields in sidecars (see Stack Reconstruction on Rebuild).

---

## On-Client Filesystem

*Desktop clients only. Mobile clients (Android/iOS) use platform-sandboxed storage and are handled separately.*

### Design Priorities

**Performance**: SQLite index caches queries but is rebuildable. Thumbnails sharded to avoid directory entry limits on large libraries.

### Client Layout

```text
{library_root}/
├── media/
│   └── {YYYY}/
│       └── {YYYY-MM}/
│           ├── {uuid}.{ext}           # Media file (read-only after commit)
│           └── {uuid}.cbor            # Sidecar metadata (mutable)
├── index/
│   ├── library.sqlite                 # Rebuildable query cache
│   ├── meta/
│   │   └── {uuid[0:2]}/{uuid[2:4]}/{uuid}.meta.cbor   # Full parsed metadata (ephemeral)
│   ├── transcodes/
│   │   ├── h264/
│   │   │   └── {uuid[0:2]}/{uuid[2:4]}/{uuid}.mp4     # H.264/MP4 transcode (ephemeral)
│   │   └── live/
│   │       └── {uuid[0:2]}/{uuid[2:4]}/{uuid}.mov      # Live Photo video (ephemeral)
│   └── thumbnails/
│       └── {xs|s|m|l|xl|o}/
│           └── {uuid[0:2]}/{uuid[2:4]}/
│               ├── {uuid}.jxl         # JXL (default)
│               └── {uuid}.webp        # WebP (fallback)
└── .library/
    ├── version.cbor                   # Library schema version
    ├── config.cbor                    # User preferences and library state
    ├── lock                           # Process lock file (ephemeral)
    ├── trash/
    │   └── {uuid}.{ext}               # Soft-deleted media (30-day quarantine)
    └── migrations/                    # Schema upgrade scripts
```

### File Naming and Placement

**Naming**: `{UUIDv7}.{extension}` — lowercase UUID, lowercase extension.

- UUIDv7 is time-ordered, sortable, and globally unique.
- Extension is the original file extension, lowercased (e.g., `.jpg`, `.arw`).
- All UUIDs and path components are always lowercase. Never mix case.

**Placement**: `media/{YYYY}/{YYYY-MM}/` based on the media capture timestamp (see EXIF Timezone Resolution for the fallback chain).

**Filesystem requirements**: FAT32 is not supported (4 GB file limit, 512-entry directory limit). exFAT, APFS, NTFS, and any modern Linux filesystem are supported. Filesystem type is not checked at runtime.

**Case sensitivity**: macOS defaults to case-insensitive. Always write lowercase; query case-insensitively.

---

## Supported File Formats

Capsule accepts all formats supported by [rawshift](https://github.com/justin13888/rawshift#format-support), the decoding and thumbnail-generation backend.

At import time, files are filtered by extension (case-insensitive). Files with unrecognized extensions are skipped and included in the import summary as `Unsupported`. XMP sidecar files (`.xmp`) are a recognized extension and are handled as stack members (see Stack Detection and Pairing). ZIP archives and other container formats are not extracted.

---

## Data Model

### Asset-to-File Model

**Every file is its own asset.** Each imported file receives a unique UUID, a CBOR sidecar, and an `assets` row. Related files (e.g., a RAW+JPEG pair, a burst sequence) are connected via the **stack relationship layer** — a separate set of tables (`asset_stacks`, `stack_members`) — not by collapsing them into a single asset.

This model is consistent with the Recovery-First principle: every file is independently recoverable from its sidecar alone, with no dependency on a companion file.

**Version prioritization**: Which file to show in the grid for a collapsed stack is determined by `asset_stacks.cover_asset_id` (user-overridable) falling back to `asset_stacks.primary_asset_id` (set at detection time). The `member_role` and `sequence_order` fields on `stack_members` encode the semantic relationship and display order within an expanded stack. See Timeline Ordering and Stack Display.

### CBOR Sidecar Schema

Every `.cbor` sidecar must include the fields below. Fields marked **REQUIRED** must be non-null on every write for new imports. Fields marked **WRITE-ONCE** are set at import and must not be overwritten on subsequent metadata updates, even if the caller provides a new value. Nullable fields may be absent on read — sidecars written by an older version will not have fields added in later versions.

```text
version              u8       REQUIRED. Schema version. Current: 1.
uuid                 string   REQUIRED. UUIDv7 of this asset (matches filename stem).
asset_type           string   REQUIRED. "photo" | "video" | "motion_photo".
original_filename    string   REQUIRED. Filename at import time (e.g., "IMG_1234.JPG").
import_timestamp     i64      REQUIRED. Unix epoch seconds UTC. When this file was imported.
modified_timestamp   i64      REQUIRED. Unix epoch seconds UTC. Last metadata edit time.
                              Set to import_timestamp at import. Updated on every sidecar write.
hash_blake3          string   REQUIRED. 64-char lowercase hex. BLAKE3 hash of the media file bytes.
file_size            u64      REQUIRED. Byte size of the media file.
is_deleted           bool     REQUIRED. Soft-delete flag. true = in trash.
rating               u8       REQUIRED. 0–5 star rating. 0 = unrated.
tags                 [string] REQUIRED. User-assigned tags. Empty list if none.
import_mode          string   REQUIRED. WRITE-ONCE. "copy" | "move". Source handling at import.
importer_version     string   REQUIRED. WRITE-ONCE. Semver of Capsule client that imported this file.
rawshift_version     string   REQUIRED. WRITE-ONCE. Semver of rawshift at import time.

capture_timestamp    i64      Nullable. Local wall-clock time stored as if it were Unix epoch
                              (no UTC offset applied). Fallback chain: EXIF DateTimeOriginal →
                              EXIF DateTime → file mtime → import_timestamp.
                              WARNING: Do NOT use for timeline sorting. Use capture_utc instead.
                              Preserved for repair and display of the raw EXIF value.
capture_utc          i64      Nullable. Capture time as Unix epoch seconds UTC.
                              Null when capture_tz_source = "floating".
capture_tz           string   Nullable. IANA timezone name (e.g., "America/New_York") or UTC
                              offset string (e.g., "+09:00"). Null = floating/unknown.
capture_tz_source    string   Nullable. "offset_exif" | "gps_lookup" | "floating".
tz_db_version        string   Nullable. IANA tz-db release tag (e.g., "2024b") used for GPS
                              lookup. Non-null only when capture_tz_source = "gps_lookup".

width                u32      Nullable. Pixel width. Null for unknown/corrupt files.
height               u32      Nullable. Pixel height. Null for unknown/corrupt files.
duration_ms          u64      Nullable. Duration in milliseconds. Null for photos.

stack_hint           map      Nullable. Mutable. Stack reconstruction hint used to rebuild
                              stack relationships during a full index rebuild.
                              Null if this asset has never been part of a stack.
                              Set at import for auto-detected stacks. Written or updated
                              when the user manually stacks, re-stacks, or changes roles.
                              Set to null when the user dissolves a stack.
  .detection_key     string   REQUIRED if stack_hint present. Shared key used to group
                              peers during rebuild. Always lowercase.
                              - "filename_stem": lowercase stem (e.g., "img_1234")
                              - "content_identifier": Apple Live Photo UUID string
                              - "timecode": shared SMPTE timecode string
                              - "manual": UUIDv7 of the stack (stable, assigned at stack creation)
  .detection_method  string   REQUIRED if stack_hint present.
                              "filename_stem" | "content_identifier" | "timecode" | "manual"
  .member_role       string   REQUIRED if stack_hint present. Role of this file within its stack.
                              "primary" | "raw" | "video" | "audio" | "depth_map" | "processed"
                              | "source" | "alternate" | "sidecar" | "proxy" | "master"
  .stack_type        string   REQUIRED if stack_hint present. Classification of the stack.
                              "raw_jpeg" | "burst" | "live_photo" | "portrait"
                              | "smart_selection" | "hdr_bracket" | "focus_stack"
                              | "pixel_shift" | "panorama" | "proxy" | "chaptered"
                              | "dual_audio" | "custom"

album_id             string   Nullable. Album UUID. Null if not assigned.
                              One album per asset; multi-album support is a future concern.

deleted_at           i64      Nullable. Unix epoch seconds UTC. Null if not deleted.

camera_make          string   Nullable. EXIF Make (e.g., "Apple"). Null if absent.
camera_model         string   Nullable. EXIF Model (e.g., "iPhone 15 Pro"). Null if absent.
gps_lat              f64      Nullable. Decimal degrees latitude. Null if absent.
gps_lon              f64      Nullable. Decimal degrees longitude. Null if absent.
```

**Forward compatibility**: Unknown fields encountered on read must be preserved verbatim on write. Never deserialize into a strict struct that drops unknown keys — use a map-based merge strategy.

**`stack_hint` mutability rules**: All `stack_hint` writes follow the atomic temp + rename sidecar write pattern. When multiple assets in a stack are affected by a single operation (manual stack, re-stack, dissolve), all their sidecars are updated atomically as a group — write all `.cbor.tmp` files first, then rename each in sequence. If any rename fails, delete all `.tmp` files and do not commit any writes. See Stack Metadata Writes.

### Stack Reconstruction on Rebuild

When rebuilding the index from sidecars (empty DB + sidecars present, or explicit repair):

1. Scan all `media/**/*.cbor` sidecars.
2. For each sidecar with a non-null `stack_hint`: group by `(detection_key, detection_method)`.
3. For each group: create one `asset_stacks` row using the `stack_type` from the hint. The file with `member_role: "primary"` becomes `primary_asset_id`; if no primary is present, use the file with the lowest `capture_utc`. Set `is_auto_generated` based on whether `detection_method` is `"manual"` (false) or any other value (true).
4. Insert one `stack_members` row per file in the group with `sequence_order`, `member_role` from the hint.
5. Recompute `is_stack_hidden` for all stack members (see Timeline Ordering and Stack Display).
6. Assets with `stack_hint: null` are imported as standalone (no stack row created).

### Comprehensive Metadata Cache (`.meta.cbor`)

Full decoded metadata for formats that produce verbose parsed output (RAW files especially) is stored at:

- Client: `index/meta/{uuid[0:2]}/{uuid[2:4]}/{uuid}.meta.cbor`
- Server: `cache/meta/{uuid[0:2]}/{uuid[2:4]}/{uuid}.meta.cbor`

These files are **ephemeral** — deleted and regenerated at any time, including on app upgrade if the parser version changes.

**Why separate from the sidecar**: Raw camera metadata can be hundreds of fields per file and is parser-version-sensitive. Keeping it separate avoids sidecar bloat and prevents parser changes from requiring sidecar migrations.

**Generation timing**: Deferred — generated on first access (e.g., when the client opens the detail view). Not generated during import. Background pre-generation is permitted as a low-priority idle task after import completes but must not block import progress reporting.

**Authority**: `.meta.cbor` is a pure cache. Parser-version-sensitive fields (verbose EXIF, proprietary maker notes) never get promoted to the sidecar. Each device generates `.meta.cbor` independently from the media file using its local rawshift version. `.meta.cbor` is never synced between devices.

#### `.meta.cbor` Schema

```text
version          u8       REQUIRED. Schema version. Current: 1.
uuid             string   REQUIRED. UUIDv7 of the asset this metadata belongs to.
schema           string   REQUIRED. Parser family identifier.
                          Format: "capsule.meta.<type>.<subversion>"
                          Examples: "capsule.meta.exif.v1", "capsule.meta.raw.v1"
                          Used to detect stale caches from a different parser generation.
rawshift_version string   REQUIRED. Semver of rawshift used to generate this file.
generated_at     i64      REQUIRED. Unix epoch seconds UTC. When this file was generated.
```

All remaining fields are schema-specific. Unknown fields must not be interpreted by code that does not recognize the `schema` value.

**Stale detection**:

- If `schema` does not match the current app's expected schema string → treat as missing, regenerate.
- If `rawshift_version` **major** component differs from current rawshift major → regenerate.
- Minor/patch differences: preserve (rawshift maintains semver compat within a major).

### Library Config Schema (`.library/config.cbor`)

```text
schema_version    u8     REQUIRED. Config schema version. Current: 1.
library_name      string REQUIRED. User-visible name for this library.
last_opened_at    i64    REQUIRED. Unix epoch seconds UTC. Updated on every library open.
last_scrubbed_at  i64    Nullable. Unix epoch seconds UTC. Updated after each .tmp cleanup.
                         Null if never scrubbed.
```

`last_opened_at` triggers a full index rebuild on startup if >30 days have elapsed, indicating the index may have drifted from external edits or OS file operations.

`last_scrubbed_at` controls the 7-day cooldown for `.tmp` cleanup scans (see Temp File Staging and Recovery).

### SQLite Index Schema

The index is fully rebuildable by scanning `media/**/*.cbor` sidecars. Schema is intentionally minimal — only fields required for querying, browsing, and duplicate detection. Verbose metadata lives in `.meta.cbor`, not here.

```sql
CREATE TABLE assets (
    uuid              TEXT    PRIMARY KEY,
    asset_type        TEXT    NOT NULL,           -- 'photo' | 'video' | 'motion_photo'
    capture_timestamp INTEGER NOT NULL,           -- Local wall-clock as epoch; NOT UTC
    capture_utc       INTEGER,                    -- Unix epoch seconds UTC; null if floating
    capture_tz_source TEXT,                       -- 'offset_exif' | 'gps_lookup' | 'floating'
    import_timestamp  INTEGER NOT NULL,           -- Unix epoch seconds UTC
    hash_blake3       TEXT    NOT NULL,
    width             INTEGER,
    height            INTEGER,
    duration_ms       INTEGER,                    -- null for photos
    stack_id          TEXT,                       -- FK to asset_stacks.id; null if unstacked
    is_stack_hidden   INTEGER NOT NULL DEFAULT 0, -- 1 = hidden in collapsed stack view
    chromahash         TEXT,                       -- base64 Chromahash; null if not generated
    dominant_color    TEXT,                       -- '#rrggbb' hex; null if not generated
    album_id          TEXT,
    rating            INTEGER NOT NULL DEFAULT 0,
    is_deleted        INTEGER NOT NULL DEFAULT 0, -- 0/1 boolean
    deleted_at        INTEGER                     -- null if not deleted
);

CREATE TABLE asset_stacks (
    id                TEXT    PRIMARY KEY,        -- UUIDv7
    stack_type        TEXT    NOT NULL,           -- StackType value (e.g., 'raw_jpeg')
    primary_asset_id  TEXT    NOT NULL REFERENCES assets(uuid),
    cover_asset_id    TEXT    REFERENCES assets(uuid),
    is_collapsed      INTEGER NOT NULL DEFAULT 1, -- 0/1; 1 = collapsed in grid view
    is_auto_generated INTEGER NOT NULL DEFAULT 1, -- 0/1; 0 = manually created
    created_at        INTEGER NOT NULL,           -- Unix epoch seconds UTC
    modified_at       INTEGER NOT NULL            -- Unix epoch seconds UTC
);

CREATE TABLE stack_members (
    id                TEXT    PRIMARY KEY,        -- UUIDv7
    stack_id          TEXT    NOT NULL REFERENCES asset_stacks(id),
    asset_id          TEXT    NOT NULL REFERENCES assets(uuid),
    sequence_order    INTEGER NOT NULL,           -- Display order within expanded stack
    member_role       TEXT    NOT NULL,           -- MemberRole value (e.g., 'primary', 'raw')
    created_at        INTEGER NOT NULL,           -- Unix epoch seconds UTC
    UNIQUE (stack_id, asset_id)
);

CREATE TABLE asset_tags (
    uuid  TEXT NOT NULL REFERENCES assets(uuid),
    tag   TEXT NOT NULL,
    PRIMARY KEY (uuid, tag)
);

-- Core asset indices
CREATE INDEX idx_assets_hash            ON assets(hash_blake3);
CREATE INDEX idx_assets_utc             ON assets(capture_utc, capture_timestamp);
CREATE INDEX idx_assets_deleted         ON assets(is_deleted);
CREATE INDEX idx_assets_album           ON assets(album_id);
CREATE INDEX idx_assets_stack           ON assets(stack_id);
-- Composite index for the main timeline query (is_deleted=0, is_stack_hidden=0)
CREATE INDEX idx_assets_timeline        ON assets(is_deleted, is_stack_hidden, capture_utc, capture_timestamp);

-- Stack indices
CREATE INDEX idx_stacks_type            ON asset_stacks(stack_type);
CREATE INDEX idx_stacks_primary         ON asset_stacks(primary_asset_id);
CREATE INDEX idx_stack_members_stack    ON stack_members(stack_id);
CREATE INDEX idx_stack_members_asset    ON stack_members(asset_id);

CREATE INDEX idx_tags_tag               ON asset_tags(tag);
```

Schema version is stored via `PRAGMA user_version = 1`. Increment on any structural change. Because the index is always rebuildable, migrations may drop and rebuild rather than `ALTER TABLE`.

`capture_utc` is preferred for timeline sorting. Fall back to `capture_timestamp` only when `capture_tz_source = 'floating'` (UTC is unknowable).

---

## Import Pipeline

Import is a four-phase pipeline. Each phase produces a typed result passed to the next. The pipeline is fully offline — no network calls at any point.

### Phase 1 — Scan (fast, read-only, no hashing)

- **Input**: One or more file or directory paths from the user.
- **Directory input**: Recursive traversal, extension-filtered (case-insensitive).
- **Per file**: Parse header via `capsule-media` → `MotionPhotoInfo { format, content_identifier }`; detect Apple Live Photo pairs within the same source directory by `content_identifier`; detect stacks by filename stem and file type (see Stack Detection and Pairing).
- **Output**: `ScanResult { candidates: Vec<ImportCandidate> }` where each `ImportCandidate` is:
  ```
  ImportCandidate {
      source_paths:     Vec<Path>,
      detected_type:    AssetType,
      stack_type:       Option<StackType>,    // None if standalone
      detection_method: Option<DetectionMethod>, // None if standalone
      detection_key:    Option<String>,       // None if standalone
      members:          Vec<(Path, MemberRole)>,  // role per file
  }
  ```
  `StackType` and `MemberRole` map directly to the server enum values (`"raw_jpeg"`, `"live_photo"`, etc.).
- Cancellable between files.

### Phase 2 — Plan (hashing, I/O-intensive)

- For each candidate: compute BLAKE3 of source file(s) → query SQLite for duplicates (Phase A hash — see BLAKE3 Hash Timing).
- Determine action per candidate: `Import | SkipDuplicate | SkipUnsupported | SkipError`.
- If `target_album_id` is provided: validate it exists in SQLite. If not found, fail the entire plan before any files are copied.
- **Output**: `ImportActionPlan { actions: Vec<ImportAction>, counts: PlanCounts }`.
- Cancellable between files.

### Phase 3 — Review (UI, no side effects)

Caller (UI layer) receives `ImportActionPlan` and presents counts. User confirms or cancels; optionally selects target album, `import_mode`, and whether to force-import duplicates.

### Phase 4 — Execute (commit, streaming progress)

Walks `ImportActionPlan`; commits each `Import` action per the atomic two-phase commit spec below. Emits `ImportProgressEvent` per file. Produces `ImportExecutionSummary` at completion.

#### Atomic Two-Phase Commit (per file)

```text
1. Generate UUIDv7.
2. Create target directory if needed (idempotent mkdir).
3. Copy source to {uuid}.{ext}.tmp (same directory as the committed file).
4. Compute BLAKE3 of .tmp → verify equals source_hash (Phase B integrity check).
   On mismatch: delete .tmp, record CorruptTransfer, skip this file.
5. Build sidecar struct with all REQUIRED fields populated.
   If this file has a detected stack: populate stack_hint.
6. Write sidecar to {uuid}.cbor.tmp.
7. Atomic rename {uuid}.{ext}.tmp → {uuid}.{ext}.
8. Atomic rename {uuid}.cbor.tmp → {uuid}.cbor.
9. Insert row into SQLite assets table. Failure here = stale cache only; file is already committed.
10. Update stack tables in SQLite (see Stack Index Updates).
```

**Invariant**: Both files commit or neither does. If step 6 fails, delete `{uuid}.{ext}.tmp` before returning. If step 7 succeeds but step 8 fails, the media file exists without a sidecar (orphaned) — log as `OrphanedMedia`; attempt cleanup on next startup scrub.

**Move mode**: Delete source files only after step 10 succeeds for all files in the group (see Copy vs. Move Policy).

#### Stack Index Updates (step 10)

After the `assets` row is inserted, update stack tables if the file belongs to a stack:

```text
1. Check if an asset_stacks row exists for this (detection_key, detection_method) pair
   by looking up stack_members for the existing candidates in this batch.
2. No existing stack:
   - Create asset_stacks row: id = new UUIDv7, stack_type from candidate,
     primary_asset_id = this UUID (tentative), cover_asset_id = null initially,
     is_collapsed = 1, is_auto_generated = 1, created_at = now, modified_at = now.
   - Insert stack_members row: stack_id, asset_id, sequence_order = 0, member_role.
   - This file is the cover for now; set is_stack_hidden = 0.
3. Existing stack:
   - Insert stack_members row with next available sequence_order.
   - If member_role = "primary": update asset_stacks.primary_asset_id.
   - If no cover_asset_id set and member_role = "primary": update cover_asset_id too.
   - Non-cover member: set is_stack_hidden = 1 on this asset's row.
4. Finalize cover/primary after all batch files are committed:
   - Prefer the file with member_role = "primary" as both primary_asset_id and cover_asset_id.
   - If multiple primaries: use the one with the lowest sequence_order.
   - Recompute is_stack_hidden for all stack members in the batch.
```

---

## Import Details

### Copy vs. Move Policy

The pipeline accepts an explicit `import_mode` parameter: `"copy"` (default) or `"move"`.

**Copy**: Source files are never touched. Safe for read-only media (SD cards, network shares, shared folders).

**Move**: Source files are deleted only after the destination is fully committed — both renames and the SQLite insert have succeeded. Never delete a source file speculatively or mid-flight.

**Move with stacks**: All files in the stack must be individually committed (through step 10) before any source in the stack is deleted. If the stack commits partially (one file fails), no sources are deleted.

`import_mode` is WRITE-ONCE in the sidecar and preserved on all subsequent metadata updates.

### BLAKE3 Hash Timing and Copy Integrity

Two sequential hash phases are required for every imported file.

**Phase A — Duplicate detection** (before any library I/O):

```text
1. Open source file.
2. Compute BLAKE3 → source_hash.
3. Query SQLite WHERE hash_blake3 = source_hash.
4. If match found → handle per duplicate policy; skip copy.
```

**Phase B — Copy integrity verification** (after writing `.tmp`):

```text
5. Copy source to {uuid}.{ext}.tmp.
6. Compute BLAKE3 of .tmp → dest_hash.
7. If source_hash != dest_hash:
       delete .tmp
       record CorruptTransfer
       skip this file — do not proceed
8. hash_blake3 stored in sidecar = source_hash (verified equal to dest_hash).
```

Phase A hashes the source on the source filesystem (works for read-only media). Phase B hashes the destination `.tmp` on the library filesystem, catching silent bitrot or hardware faults during the copy. A file whose integrity cannot be verified is never committed.

### Duplicate Detection

At Phase A, if a matching `hash_blake3` is found in SQLite:

- **Default**: Skip import; log existing asset UUID in summary as `DuplicateSkipped`.
- **Force re-import** (user-triggered via Phase 3): Assign a new UUID and import as a separate asset.

Duplicates do not block bulk imports. They are resolved per-candidate in Phase 2 and reported in the final summary.

### Stack Detection and Pairing

Stack detection during the scan phase identifies multi-file relationships and classifies them into the `StackType` taxonomy. Detection is best-effort and fully offline. Cross-directory pairing is not performed.

#### Stack Type Reference

| Stack Type        | `stack_type` value  | Detection method       | Detected by                                          | Valid member roles                              |
| ----------------- | ------------------- | ---------------------- | ---------------------------------------------------- | ----------------------------------------------- |
| RAW + JPEG        | `raw_jpeg`          | `filename_stem`        | Matching stem with RAW + primary extension           | `primary`, `raw`, `sidecar`                     |
| Burst             | `burst`             | `filename_stem`        | Sequential stems + EXIF burst sequence metadata      | `primary`, `alternate`                          |
| Live Photo        | `live_photo`        | `content_identifier`   | Apple `content_identifier` matching HEIC + MOV       | `primary`, `video`                              |
| Portrait / Depth  | `portrait`          | `filename_stem`        | Depth map companion (`.heic` + depth-flagged variant)| `primary`, `depth_map`                          |
| Smart Selection   | `smart_selection`   | `manual` (AI-created)  | AI-similarity grouping (post-import AI pipeline)     | `primary`, `alternate`                          |
| HDR Bracket       | `hdr_bracket`       | `filename_stem`        | Sequential stems + EXIF EV values (±EV bracket set) | `source`, `processed`                           |
| Focus Stack       | `focus_stack`       | `filename_stem`        | Sequential stems + EXIF focus distance progression  | `source`, `processed`                           |
| Pixel Shift       | `pixel_shift`       | `filename_stem`        | Proprietary maker note (Olympus, Sony) pixel shift  | `source`, `processed`                           |
| Panorama          | `panorama`          | `filename_stem`        | Sequential stems + EXIF panorama sequence metadata  | `source`, `processed`                           |
| Proxy / Optimized | `proxy`             | `filename_stem`        | Matching stem with master (8K RAW) + proxy (HD)     | `master`, `proxy`                               |
| Chaptered Video   | `chaptered`         | `filename_stem`        | GoPro/action cam chapter naming (`GOPR001`, `GP001`)| `source` (each chapter)                         |
| Dual-System Audio | `dual_audio`        | `timecode`             | Shared SMPTE timecode (video + external WAV/AIFF)   | `primary`, `audio`                              |
| Manual            | `custom`            | `manual`               | User-created in UI                                  | Any; user-assigned                              |

**RAW extensions** (`member_role: "raw"`):

```text
ARW, CR2, CR3, NEF, NRW, RW2, ORF, PEF, RAF, SRW,
3FR, DCR, DNG, ERF, MEF, MOS, MRW, PTX, RWL, X3F
```

DNG is classified as raw even though it is a standardized format — it is typically the lossless capture file and the JPEG is the display file.

**Primary extensions** (`member_role: "primary"` in a `raw_jpeg` stack):

```text
JPG, JPEG, HEIC, HEIF, AVIF, PNG, TIFF, TIF
```

**XMP sidecar extension** (`member_role: "sidecar"`):

```text
XMP
```

**Pairing rules (RAW+JPEG)**:

- Match by identical filename stem, case-insensitive, within the same source directory.
- One primary + one or more raws → all share a stack; primary has `member_role: "primary"`.
- Multiple raw formats for the same stem (e.g., `.ARW` + `.DNG` + `.JPG`) → all imported into the same stack; each raw gets `member_role: "raw"`.
- No primary found for a raw → raw imported as standalone (`stack_hint: null`).
- XMP paired to the raw of the same stem; if no raw exists, XMP imports standalone.
- Apple Live Photo pairing (by `content_identifier`) is handled separately and is not stem-based.

**XMP sidecar handling**: An `.xmp` file receives its own UUID and is stored at `media/{YYYY}/{YYYY-MM}/{uuid}.xmp`. It shares the `stack_hint.detection_key` of the RAW it is paired with, with `member_role: "sidecar"` and `stack_type: "raw_jpeg"`. XMP content is not parsed into the Capsule sidecar — preserved verbatim as an opaque binary blob for third-party tool compatibility (Lightroom, Capture One, etc.). Capsule never modifies the XMP file's content.

### Stack Metadata Writes

Edits (tags, rating, album) applied to a stack are written atomically:

1. Write all updated sidecars to `.tmp` files.
2. Atomic rename each `.tmp` → final in sequence.
3. If any rename fails: delete all `.tmp` files; do not commit any writes in the stack.
4. Update SQLite for all rows after all renames succeed.

**Manual stack/re-stack/dissolve**: Updating `stack_hint` across multiple sidecars follows the same atomic group write pattern. A dissolve sets `stack_hint` to null on all affected sidecars and removes the `asset_stacks` and `stack_members` rows from SQLite. A re-stack (moving an asset from one stack to another) updates the sidecar's `stack_hint` and migrates `stack_members` rows.

### Partial Stack Duplicate Handling

| Primary state | RAW state | Action                                                                                   |
| ------------- | --------- | ---------------------------------------------------------------------------------------- |
| Duplicate     | Duplicate | Skip both. Log both existing UUIDs.                                                      |
| Duplicate     | New       | Import RAW as standalone (`stack_hint: null`). Log skipped primary.                     |
| New           | Duplicate | Import primary as standalone. Log skipped RAW.                                           |
| New           | New       | Standard stack import.                                                                   |

For stacks of 3+ files: same logic — any non-duplicate member is imported as a standalone asset.

Never link a new file into an existing stack — that would require modifying the existing stack's `primary_asset_id` or cover selection, and silently mutating another user's import. The user must manually merge stacks in the UI.

Summary outcome: `PartialStackImported` — includes imported UUID(s) and skipped duplicate UUID(s).

### Timeline Ordering and Stack Display

**Sorting key**: `capture_utc` (preferred), falling back to `capture_timestamp` when `capture_tz_source = 'floating'`.

**Collapsed stacks in the timeline**: When a stack has `is_collapsed = 1`, only the cover asset represents the stack in the timeline grid. The cover is determined by:
1. `asset_stacks.cover_asset_id` if explicitly set (user override).
2. Otherwise, `asset_stacks.primary_asset_id`.

All other stack members have `is_stack_hidden = 1` on their `assets` row and are excluded from the main timeline query.

**Timeline query** (efficient):

```sql
SELECT * FROM assets
WHERE is_deleted = 0 AND is_stack_hidden = 0
ORDER BY COALESCE(capture_utc, capture_timestamp) DESC
LIMIT ? OFFSET ?;
```

The composite index `idx_assets_timeline ON assets(is_deleted, is_stack_hidden, capture_utc, capture_timestamp)` makes this efficient even for multi-million-asset libraries.

**Stack expansion**: When the user expands a stack in the UI, query:

```sql
SELECT a.* FROM stack_members sm
JOIN assets a ON sm.asset_id = a.uuid
WHERE sm.stack_id = ?
ORDER BY sm.sequence_order ASC;
```

Display all members inline in the expanded area. `is_stack_hidden` is not consulted for this query — it is only a timeline optimization.

**Maintaining `is_stack_hidden`**: This flag is a denormalized cache. It is recomputed whenever:
- A stack is created or dissolved.
- `is_collapsed` changes on a stack.
- `primary_asset_id` or `cover_asset_id` changes.
- A member is added, removed, or reassigned.

The flag is never stored in the sidecar — it is a pure DB optimization. On index rebuild, it is recomputed from the reconstructed stack state.

**Uncollapsed stacks** (`is_collapsed = 0`): All members have `is_stack_hidden = 0` and appear individually in the timeline at their own `capture_utc` positions. The UI visually groups them (shared border, stack count badge) but each occupies its own grid cell.

### Motion Photo Detection

**Detection algorithm** (fully offline, via `capsule-media`):

```text
1. Parse candidate file via capsule-media → MotionPhotoInfo { format, content_identifier }.
2. Dispatch on format:
   - GoogleMicroVideo  → single file, self-contained; asset_type = "motion_photo"
   - SamsungMotion     → single file, self-contained; asset_type = "motion_photo"
   - AppleLivePhoto    → still component; resolve video companion (see below)
   - Unknown           → treat as regular photo/video based on extension
```

**Apple Live Photo pairing** (scan phase):

```text
1. Extract content_identifier from .HEIC
   (XMP field: com.apple.quicktime.content.identifier).
2. Scan the same source directory for a .MOV with a matching content_identifier.
3. Paired .MOV found   → treat the pair as a single motion_photo asset with:
                          stack_type = "live_photo", detection_method = "content_identifier",
                          detection_key = content_identifier value.
                          HEIC gets member_role = "primary", MOV gets member_role = "video".
4. No .MOV found       → import .HEIC as asset_type = "photo" (still only).
                          Log outcome as LivePhotoWithoutPair.
```

**Storage by format**:

| Format               | Primary file                  | Video component                                  | Sidecars |
| -------------------- | ----------------------------- | ------------------------------------------------ | -------- |
| Google MicroVideo    | `{uuid}.jpg` (self-contained) | Embedded — no separate file                      | 1        |
| Samsung Motion (SEF) | `{uuid}.jpg`                  | Embedded                                         | 1        |
| Apple Live Photo     | `{uuid}.heic`                 | `transcodes/live/{shard}/{uuid}.mov` (ephemeral) | 1        |

The Apple Live Photo `.MOV` is stored at `index/transcodes/live/{uuid[0:2]}/{uuid[2:4]}/{uuid}.mov` (client) or `cache/transcodes/live/…` (server). It is ephemeral — deleted and re-sourced on re-import or server fetch. The `live/` subdirectory separates it from the H.264/MP4 transcode at `transcodes/h264/{shard}/{uuid}.mp4`; path derivation for both is unambiguous from UUID + type alone.

**Thumbnails**: Generated from the still frame using the standard thumbnail pipeline. No special handling.

**Playback**:

| Option     | Container                    | Codec                | When                 |
| ---------- | ---------------------------- | -------------------- | -------------------- |
| Original   | MOV / HEIC / vendor-specific | HEVC or vendor codec | Platform supports it |
| Transcoded | MP4                          | H.264 (AVC)          | Universal fallback   |

H.264/MP4 was chosen over AV1/VP9: motion photos are short, low-bitrate clips where universal hardware decode support outweighs compression efficiency.

### Album Assignment at Import Time

The pipeline accepts an optional `target_album_id: Option<Uuid>`. If provided, all committed assets in the batch receive `album_id = target_album_id`. Stack members all receive the same `album_id`.

**Constraints**:

- The album must already exist in the library. Validated in Phase 2 — if not found in SQLite, the entire plan fails before any files are copied.
- No album is auto-created during import.
- `target_album_id` is uniform across the batch; per-file album assignment is not supported at import time.
- Duplicate-skipped files are not re-assigned — the existing asset's `album_id` is not modified.

### Import Cancellation

Cancellation is honoured only between files, not mid-file. When the user cancels:

1. Finish writing the current file pair (`{uuid}.{ext}` + `{uuid}.cbor`) to completion.
2. Do not start the next file.
3. All fully committed files remain in the library.
4. Update the SQLite index for committed files before returning.

Partial stacks (e.g., RAW written but JPEG cancelled) are committed as-is — the RAW becomes a standalone asset with `stack_hint: null`. The partial `asset_stacks` row (if created) is cleaned up: if only one member remains after cancellation, dissolve the stack and update that member to standalone.

### Import Progress and Error Reporting

The pipeline emits typed progress events through a channel or callback for real-time UI updates. Errors are surfaced as events, not buffered to the end.

**Event types**:

```text
ImportStarted    { total_files: u64, total_bytes: u64 }
FileStarted      { index: u64, total: u64, source_path: string }
FileCompleted    { index: u64, uuid: string, outcome: ImportOutcome, bytes: u64 }
ImportCompleted  { summary: ImportExecutionSummary }
```

**`ImportOutcome` values**: `Imported | DuplicateSkipped | Unsupported | CorruptUnreadable | CorruptTransfer | PermissionDenied | PartialStackImported | LivePhotoWithoutPair`.

**Guarantees**:

- `total_files` counts only `Import`-action candidates; duplicates and unsupported are already resolved in Phase 2.
- `FileCompleted` is emitted for every file including errors — never silently dropped.
- `ImportCompleted` is emitted exactly once, after all files (including any stack rollbacks).
- Progress indices are 1-based and monotonically increasing — never reset on error.

**Summary outcome table**:

| Outcome                | In library          | Logged info                 |
| ---------------------- | ------------------- | --------------------------- |
| `Imported`             | Yes                 | —                           |
| `DuplicateSkipped`     | No (already exists) | Existing asset UUID         |
| `Unsupported`          | No                  | Source path                 |
| `CorruptUnreadable`    | No                  | Source path + error         |
| `CorruptTransfer`      | No                  | Source path + hash mismatch |
| `PermissionDenied`     | No                  | Source path + OS error      |
| `PartialStackImported` | Partial             | Imported + skipped UUIDs    |
| `LivePhotoWithoutPair` | Yes (as photo)      | Source path                 |

The summary is returned to the UI layer and not written to disk. Do not surface per-file dialogs during a bulk import.

---

## Operational Behaviors

### Library Initialization

When creating a new library at a given directory path:

```text
1. Verify the target directory is empty or does not exist.
   Abort if it contains unrecognized files.
2. Create directory skeleton:
   media/
   index/thumbnails/{xs,s,m,l,xl,o}/
   index/meta/
   index/transcodes/h264/
   index/transcodes/live/
   .library/migrations/
   .library/trash/
3. Initialize empty SQLite database at index/library.sqlite.
4. Write .library/version.cbor: { version: 1 }
5. Write .library/config.cbor: { schema_version: 1, last_opened_at: <now>,
                                  library_name: <user-provided>, last_scrubbed_at: null }
```

All steps must succeed atomically. If any step fails, remove all created files and directories and surface an error. Do not leave a partially initialized library on disk.

On subsequent opens: validate that `.library/version.cbor` is present and readable. If missing or unreadable, treat as a corrupt library and refuse to open — do not auto-repair silently.

### Concurrent Access and Library Locking

**Library-level locking**: `.library/lock` prevents two app instances from opening the same library simultaneously. The lock file is a JSON object (human-readable for debugging):

```text
{ "pid": <int>, "hostname": "<string>", "locked_at": <unix_epoch_seconds> }
```

**Acquire**: Create exclusively (`O_CREAT|O_EXCL` on Unix; `CreateFile` with `CREATE_NEW` on Windows). If creation fails (file already exists): read the record; if the PID is no longer alive **and** the hostname matches the current host → overwrite (stale lock recovery). Otherwise surface an error and refuse to open.

**Release**: Delete the lock file on library close. Never included in index rebuilds or library scans.

**Sidecar-level locking**: External drives may be accessed by multiple devices. Use file locking (`fcntl` on Unix, `LockFileEx` on Windows) when writing sidecars to prevent interleaved writes from concurrent processes.

### Temp File Staging and Startup Recovery

Temp files are written to the final destination directory to guarantee same-filesystem atomic renames.

```text
media/2024/2024-07/
├── {uuid}.jpg.tmp      # in-flight media
└── {uuid}.cbor.tmp     # in-flight sidecar
```

**Startup scrub**: On library open, if `last_scrubbed_at` is null or >7 days ago, scan all `media/**/*.tmp` files. Any `.tmp` file older than 5 minutes is a crashed import. Delete both `{uuid}.{ext}.tmp` and `{uuid}.cbor.tmp` for that UUID. Log the cleanup. Write updated `last_scrubbed_at` to `config.cbor`.

### Directory Auto-Creation

The library initialization skeleton creates only top-level directories. Per-date and per-UUID-prefix directories are created on demand (idempotent mkdir — no error if already exists).

| Directory                                          | Created by                                                 |
| -------------------------------------------------- | ---------------------------------------------------------- |
| `media/{YYYY}/{YYYY-MM}/`                          | Importer, before writing the first `.tmp` for that month   |
| `index/meta/{uuid[0:2]}/{uuid[2:4]}/`              | Whatever generates the first `.meta.cbor` for that prefix  |
| `index/thumbnails/{size}/{uuid[0:2]}/{uuid[2:4]}/` | Whatever generates the first thumbnail for that prefix     |
| `index/transcodes/h264/{uuid[0:2]}/{uuid[2:4]}/`   | Whatever writes the first H.264 transcode for that prefix  |
| `index/transcodes/live/{uuid[0:2]}/{uuid[2:4]}/`   | Whatever writes the first Live Photo video for that prefix |

If the app crashes after mkdir but before writing the `.tmp`, the empty directory is harmless — ignored by startup scrub and left in place.

### Index Staleness

SQLite may lag reality. Always verify file existence before operations. Trigger a full index rebuild on startup if `last_opened_at` >30 days ago or if the library reports structural inconsistencies on open.

### Soft Deletion and Trash

```text
1. Mark in SQLite: is_deleted = 1, deleted_at = <now>
2. Update sidecar: is_deleted = true, deleted_at = <now> (atomic rename pattern)
3. Move media to quarantine: .library/trash/{uuid}.{ext}
   Sidecar stays at media/{YYYY}/{YYYY-MM}/{uuid}.cbor with is_deleted = true
4. After 30-day trash period (checked at startup or on explicit purge):
   permanent deletion — remove sidecar first, then media from .library/trash/
```

**Stacked assets**: Deleting a stack member also updates `is_stack_hidden` for remaining members. Deleting the current cover or primary triggers cover reassignment: promote the next member by `sequence_order`. Deleting all members of a stack dissolves the stack (remove `asset_stacks` and `stack_members` rows).

Sidecar is removed first on permanent deletion: orphaned media in trash is recoverable (re-import); orphaned sidecars serve no purpose. Never immediate deletion — the trash period allows recovery from accidental deletes.

### EXIF Handling

EXIF is preserved in the original media file untouched. Key fields (capture date, GPS, camera model) are copied into the sidecar at import time. The sidecar is the authoritative metadata source for Capsule; EXIF in the media file is left intact for third-party tool compatibility. The media file is read-only after import — Capsule never writes to it.

### EXIF Timezone Resolution

`DateTimeOriginal` represents local wall-clock time. To establish an absolute timeline, resolve UTC + timezone at import using the following algorithm:

**Case 1 — `OffsetTimeOriginal` present**:

- `capture_tz` = offset string (e.g., `"+09:00"`)
- `capture_tz_source` = `"offset_exif"`
- `capture_utc` = `DateTimeOriginal` + offset → UTC
- `tz_db_version` = null

**Case 2 — `OffsetTimeOriginal` absent, GPS present**:

Perform a fully offline reverse-geocoded timezone lookup (see Reverse Geocoding).

- `capture_tz` = IANA timezone name (e.g., `"America/New_York"`)
- `capture_tz_source` = `"gps_lookup"`
- `capture_utc` = calculated UTC timestamp
- `tz_db_version` = IANA tz-db release tag used (e.g., `"2024b"`)

If the GPS lookup fails (ocean, Antarctica, corrupt db): fall through to Case 3. Do not fail the import.

**Case 3 — No offset, no GPS (or lookup failed)**:

- `capture_tz` = null
- `capture_tz_source` = `"floating"`
- `capture_utc` = null
- `tz_db_version` = null

**Display**: Clients must use the sidecar's stored `capture_tz` to display local capture time. Use `capture_utc` for all timeline sorting and cross-library queries. Fall back to `capture_timestamp` only when `capture_utc` is null.

**Immutability**: `capture_tz` and `capture_utc` are written once at import. If the server later derives a different timezone from a newer tz-db version, it records that in its own layer — it does not silently overwrite the sidecar's fields without an explicit user-triggered repair. `tz_db_version` makes GPS-derived zone provenance auditable.

### Reverse Geocoding (Offline)

Network calls for timezone lookup are prohibited. GPS → timezone resolution must be fully offline.

**Mechanism**: Bundle an offline timezone boundary database compiled into the binary or shipped as a read-only asset (e.g., `tzf-rs` with the IANA timezone boundary dataset for Rust). No DNS lookup, HTTP request, or IPC call to external services is permitted during import.

### Thumbnail Generation

Thumbnails are generated on-the-fly when first needed and cached locally. Not pre-generated during import.

**Formats**: Two formats per variant. Client requests JXL first, falls back to WebP.

| Format | Role     | Notes                           |
| ------ | -------- | ------------------------------- |
| JXL    | Default  | Progressive decoding; preferred |
| WebP   | Fallback | Used when JXL is unsupported    |

**Size variants**: Defined by **minor dimension** (shorter side). Ensures sufficient pixel coverage regardless of orientation.

| Variant     | Key | Minor dimension |
| ----------- | --- | --------------- |
| Micro       | xs  | 200 px          |
| Small       | s   | 450 px          |
| Medium      | m   | 900 px          |
| Large       | l   | 1500 px         |
| Extra Large | xl  | 2400 px         |
| Original    | o   | No downscale    |

**No upscaling**: If the source minor dimension is smaller than the requested variant, the `o` variant is used instead.

**Aspect ratio clamping**: Clamped to 3:1 maximum in either orientation. Panoramas and extreme crops are clamped to 3:1.

**Client size selection**: Select the smallest variant whose minor dimension meets or exceeds the display slot requirement.

- **Landscape** (width ≥ height): minor dimension = height.
- **Portrait** (height > width): minor dimension = width.

Example: a 400×300 display slot → needs ≥ 300 px minor → select `s` (450 px).

**Path**: `index/thumbnails/{size}/{uuid[0:2]}/{uuid[2:4]}/{uuid}.{format}` (client) or `cache/thumbnails/…` (server).

**Client vs. server thumbnails**: Client-generated thumbnails are cached locally and never transmitted to the server. When fetching an asset from the server, the client may seed its local thumbnail cache from the server-provided thumbnail rather than generating its own. The server generates thumbnails independently; the client never pushes locally-generated thumbnails back.

**Stacked assets**: Thumbnails are generated per-asset, not per-stack. The UI selects which asset's thumbnail to display based on the stack's cover (see Timeline Ordering and Stack Display). Thumbnails for `is_stack_hidden` members are generated lazily on stack expansion — not eagerly.

### LQIP (Low-Quality Image Placeholder)

LQIP provides an instant visual placeholder before the full thumbnail loads. The implementation uses **ThumbHash** — a compact (~28 byte) perceptual encoding that stores a blurred preview, approximate aspect ratio, and average color in a single byte sequence.

**Server-side generation**: After an asset is uploaded and committed, the server generates its LQIP as a background task:

1. Decode the media file to an RGBA buffer via `capsule-media`.
2. Call `capsule-media::image::lqip::LQIP::from_image_buffer` — internally resizes to a maximum of 100 px on the longest dimension, then calls `thumbhash::rgba_to_thumb_hash`.
3. Extract dominant color via `LQIP::average_rgba()` → convert to `#rrggbb` hex.
4. Base64-encode the raw ThumbHash bytes.
5. Store in `assets.chromahash` (varchar, base64) and `assets.dominant_color` (varchar, hex). Both are nullable — null means not yet generated.

**Client-side (local library)**: LQIP generation is **skipped**. The client already has the full-resolution file locally and generates thumbnails on demand. The LQIP overhead is unnecessary when the source file is already accessible.

**Client-side (synced library)**: The client fetches `chromahash` and `dominant_color` from the server as part of asset metadata. These values are stored in the local SQLite `assets` table. The client does not regenerate LQIP locally.

**Sidecar**: LQIP is **not** stored in the sidecar. It is an ephemeral derived value (like thumbnails and `.meta.cbor`) that can be regenerated from the media file at any time. Storing it in the sidecar would bloat permanent archival metadata with a cache artifact.

**Dominant color fallback**: While the ThumbHash decodes asynchronously in the browser, `dominant_color` is available immediately as a CSS background-color — visible even before the LQIP image renders.

**API exposure**: `chromahash` and `dominant_color` are exposed on the GraphQL `AssetMetadata` type. The frontend decodes the base64 ThumbHash bytes to an RGBA bitmap using the `thumbhash` npm package (`thumbHashToRGBA`) and renders it as a blurred data-URL image.

**Stacked assets**: LQIP is stored per-asset (not per-stack). Only the cover asset's LQIP is displayed in the collapsed grid cell. No special handling is needed — the UI simply uses the cover asset's `chromahash`.
