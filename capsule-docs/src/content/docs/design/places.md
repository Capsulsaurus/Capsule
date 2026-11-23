---
title: Places
description: The location index, map clusters per zoom level computed in Rust, per-cell place names, and the requirements on the external offline geocoder
status: draft
---

Places is the map of a library and the names of where its photos were taken. Everything on this
page is **derived, local and private**: computed on the device from the effective location in each
sidecar, cached in the catalog, never written to a sidecar, never synced and never sent to the
server. Coordinates are encrypted metadata, so the server cannot compute a cluster or a place name
and is never asked to.

This page owns the location index, map clustering, the place-name cache and the requirements on
the geocoder. [](/design/metadata/#geolocation) owns how a location is stored (datum, as-imported
and user values); [](/design/search/) owns how a place is searched; [](/design/clients/) owns the
map renderer port native apps implement.

## Where It Lives

| Surface | Module | Status |
| --- | --- | --- |
| Cell math, pyramid aggregation, cluster merge, place-name lookup policy | `capsule-core::places` | Planned |
| Location index, pyramid tables, place-name cache, CLI and FFI verbs | `capsule-library::places` | Planned |
| Web Mercator, tiles, antimeridian-aware bounds, geodesy, GCJ-02 conversion | geocoordinates | Planned dependency, waiting on its next release |
| Reverse and forward geocoding | the external geocoder ([](#the-geocoder)) | Planned, separate project |

The cluster code lives in the stateless core so that the native apps (through the FFI), the CLI and
the web client (through wasm) run the same arithmetic. Apps reach it as the three verbs of the
Swift `PlacesPort`: clusters in a region at a zoom, a cluster's assets, and the library's bounding
region. The CLI exposes each with `--json` first.

**Today** none of it exists in Rust. The Swift map renders a mock with its granularity fixed at 6
(`FeatureCollections/Sources/PlacesMapView.swift:177`), Android has no map, the catalog holds the
effective location as plain `latitude`/`longitude` columns with no spatial index
(`capsule-library/src/db/schema.rs`), and place names come only from an opt-in, uncached
`CLGeocoder` call (`FeatureViewer/Sources/Info/PlaceNameResolver.swift`).

## The Grid

Every spatial structure on this page uses **one grid: Web Mercator (EPSG:3857) XYZ tiles**,
addressed as a level and a Z-order (quadkey) integer.

- **Why Mercator tiles.** Every map renderer Capsule targets draws Web Mercator, so a tile at level
  *ℓ* is a fixed number of screen pixels at every latitude. A cluster radius in pixels is then a
  fixed number of cells, which is what keeps pins evenly spaced on screen. A geohash cell is fixed
  in degrees and so stretches on screen toward the poles, and its levels step by 32 rather than 4,
  out of step with zoom. H3 hexagons do not nest exactly, so a parent's count is not the sum of its
  children's.
- **Why Z-order.** The leaves under any cell form one contiguous integer range, so "every asset in
  this cell" is a range scan on one index.
- **Leaf level 18**, about 153 m across at the equator and less toward the poles.
- **Clamp, never drop.** Mercator stops at ±85.05112878° latitude. A location beyond it is assigned
  to the edge row for gridding and keeps its real coordinate everywhere else.

## The Location Index

One row per asset that is Live, not hidden, and has an effective location
([](/design/metadata/#as-imported-and-user-values)):

- the asset id;
- the location **normalised to WGS-84**, with an `approximate` flag when it was stored as GCJ-02,
  since GCJ-02 → WGS-84 has no exact inverse ([](/design/metadata/#geolocation));
- its leaf cell;
- its effective capture time, which picks a cluster's key asset.

Hidden and trashed assets are not in it, so the map never shows them, exactly as the timeline does
not ([](/design/organization/#hidden-assets)).

**Determinism.** Normalisation and gridding use trigonometry. They MUST use the pure-Rust `libm`
implementations of every transcendental function, never the platform's, so that iOS, Android,
desktop and wasm assign every location to the same cell bit for bit. A platform `sin` that differs
in the last bit would put a location on a cell boundary in different cells on different devices.

## Clusters per Zoom

### The Pyramid

For every level 0–18, the catalog keeps one row per **occupied** cell:

- the asset count;
- the sum of its assets' unit vectors on the sphere, in fixed-point integers (2⁻³⁰), which makes
  the sum exact, independent of order, and exactly reversible on removal;
- its key asset: the newest effective capture, ties broken by the lower asset id.

A centroid from summed unit vectors stays correct across the antimeridian and near the poles, where
averaging latitudes and longitudes does not.

**Incremental.** Import, a location edit, delete, restore, hide, unhide, purge and a synced change
each update the asset's index row and the 19 pyramid rows on its old and new cell paths, **in the
same catalog transaction** as the sidecar projection. If the cell's key asset leaves, the next one
is found by a range scan over the leaf cells under it. A full rebuild from the catalog reproduces
the same rows, and the catalog checks the level-0 count against the location index on open and
rebuilds on a mismatch.

### Decision: Persisted Pyramid, On-Demand Above It

- **Zoom ≤ 16** reads the pyramid at level *z* + 2. That is 64 px cells on 256 px tiles, the
  convention the map renderer port reports zoom in ([](/design/clients/)).
- **Zoom > 16** covers a few square kilometres at most, so it clusters on demand from the location
  index's exact coordinates in the viewport.

The alternatives each fail one requirement:

- **All on demand** must aggregate every asset at the world view, which is O(n) per frame.
- **An in-memory cluster tree** must be rebuilt at every launch, holds O(n) memory per level, and
  has no incremental update for edits.

The pyramid costs at most 19 rows per occupied leaf cell. A world-view query reads a handful of
rows, and an edit writes 19.

### Merging Within Blocks

A pure grid splits two pins a pixel apart across a cell edge. So the cells of a level are merged
**within their 8 × 8 block** (the level *ℓ* − 3 parent):

1. Visit the block's occupied cells in order of count (descending), then cell id.
2. Each unvisited cell absorbs every unvisited cell in the block whose centroid lies within 64 px of
   its own at the current zoom.

A cluster carries its id (the level and its lowest absorbed cell id), count, centroid, bounds and
key asset, and is marked `approximate` if any member is. Merging never crosses a block edge, so
**a block's clusters are a pure function of that block**. Panning only adds or drops whole blocks,
and pins never jump. The cost is that two pins on opposite sides of a block edge stay separate.

A query asks for the blocks covering the viewport. A viewport crosses the antimeridian as two
ranges, never one span around the globe. Output is capped at 4,096 clusters.

### Budgets

The scale benchmark ([](/development/performance/)) measures these on the parity phone. They are
unmeasured targets until it runs.

| Measure | 1,000,000 geotagged assets | 5,000,000 geotagged assets |
| --- | --- | --- |
| Clusters for any viewport at any zoom (p95, warm) | < 16 ms, one frame at 60 Hz | < 100 ms |
| Location index and pyramid on disk | < 64 MiB | < 320 MiB |
| One edit's index and pyramid update | < 2 ms | < 2 ms |
| Full rebuild of index and pyramid | < 60 s | < 5 min |
| First full place-name pass, bundled tier | < 10 min | < 30 min |

### Consistency Across Devices

The pyramid is a pure function of synced inputs (each asset's effective location, hidden flag and
lifecycle state), computed with pinned arithmetic. Two devices holding the same synced state show
the same clusters at the same viewport and zoom, whatever renderer draws them, because every
renderer reports a renderer-neutral viewport ([](/design/clients/#map-renderer-port)).

### Album and Search Maps

A map scoped to an album, a smart album or a search result is not given its own pyramid. It groups
the location index rows of that scope by cell prefix, on demand. The result is memoised per scope
and invalidated by the catalog's change counter, and it is bounded by the scope's size. An album's
place summary (its top three place names by asset count) is derived the same way from the
place-name cache.

### Datum and Basemaps

Clusters are computed in WGS-84 and positioned in the datum the renderer declares
([](/design/clients/#map-renderer-port)). For a GCJ-02 basemap (Apple Maps and Chinese providers
inside mainland China), the core converts cluster positions forward to GCJ-02, which is exact.
Positions and pins that came from a GCJ-02 location are flagged `approximate` on a WGS-84
basemap, since GCJ-02 → WGS-84 has no exact inverse.

### Renderers and Basemap Privacy

Native code only draws: MapKit on Apple platforms, the Google Maps SDK on Android for v1 behind the
renderer port ([ADR-0022](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0022-android-maps-use-the-google-maps-sdk-first-behind-a-renderer-port.md)),
and MapLibre after v1.

**Tile requests are a disclosure.** A tile-based basemap fetches the tiles covering the viewport
from its provider: Apple for MapKit, Google for the Google Maps SDK. So **the provider learns which
areas, at which zoom, the user looks at**, together with whatever the provider's SDK attaches to
its requests (on Android, the app's API key and Google Play services' device identity). Zooming
into a cluster of home photos tells the provider roughly where home is. The provider never
receives a photo's coordinates, a count, a cluster or a place name: those stay in the core and are
drawn as local overlays.

**Maps are on by default, and the disclosure is shown before the first tile.**

- **On by default.** A per-device setting, *Show maps*, starts on.
- **Told once, in context.** The first time a map would load on a device (the Places map or an info
  page's map preview), a one-time notice appears in place before any tile is requested. It names
  that device's tile provider, says that tiles reveal the area viewed and its zoom but never a
  photo's coordinates, a count or a place name, and links to the setting that turns maps off.
  Dismissing it loads the map and it is not shown again on that device.
- **Per device, never synced.** Each device has its own provider and its own notice, so a notice
  shown for Apple on an iPhone says nothing about Google on an Android phone.
- **Off means nothing is fetched.** With *Show maps* off, no renderer is created and no tile is
  requested. The Places surface shows the place list, grouped by place name and ordered by asset
  count, and the info page shows the place name and coordinates without a map. Clusters, place
  names and search all keep working, because none of them needs a basemap.

A device with no usable renderer (an Android build without Google Play services or without a Maps
API key) behaves as if maps were off. A renderer whose tiles come from the device itself (MapLibre
with offline tiles, after v1) discloses nothing and skips the notice.

### The Web Client

The web client clusters **in the browser**, running `capsule-core::places` compiled to wasm over the
locations it decrypted itself. The server never receives a coordinate, a cell id or a viewport, so
it has no places endpoint. In v1 the web client holds no keys and shows no map.

## Place Names

### Geocoded per Cell, Not per Asset

Names are resolved **per level-16 cell** (about 611 m across at the equator), not per asset. A
library of 100,000 geotagged photos typically occupies a few thousand such cells. That makes a full
pass cheap, and it gives every photo in a cell the same name, so a street never flickers between
two neighbourhood names photo to photo.

The geocoder answers a cell query with either **uniform** (one place at each admin level covers
the whole cell) or **straddles** (the deepest level at which a boundary crosses it). A straddling
cell is resolved again at levels 17 and 18, and a straddling leaf is resolved **per asset** at its
exact location. A border town is therefore named correctly per photo, and an open field costs one
lookup.

### The Cache

Place names live in a catalog table keyed by cell (or by asset, for a straddling leaf), the dataset
id, the dataset version and tier, and the language. They are:

- **never written to a sidecar.** A place name is a function of the location and a dataset version,
  not something the user said, and a sidecar is signed user metadata;
- **never synced, never backed up and never sent anywhere.** Any device can derive them offline
  from data it already has;
- **rebuildable.** Dropping the table re-derives every name offline.

### Reliability at Scale

Geocoding is a background job over the distinct occupied cells, idempotent and resumable.

- It commits in batches. A crash resumes at the first unresolved cell.
- It reports progress as typed events.
- It runs only under the same power and thermal gates as AI work ([](/design/ai/#model-batching)).
- New and moved locations enqueue their cells in the projection's transaction.
- A missing or unloadable dataset leaves names **absent, never guessed**. The UI shows coordinates,
  and the job retries when a dataset appears.

### Consistency

- Every cache entry records the dataset version that produced it, and a dataset update re-resolves
  lazily. Devices with the same dataset version and language show the same names.
- The dataset's tiers nest ([](#the-geocoder)). A device that holds only the bundled tier shows a
  coarser name, which is an ancestor of the full tier's name and never a different place.
- Names render in the app's language setting, falling back through its chain to the dataset's
  local name ([](/design/i18n/#language-and-region)). They are data, not catalog strings.

### Place Search

`near:` and free-text place words ([](/design/search/)) look first in the place-name cache, the
places that actually occur in the library, and then in the geocoder's offline forward index. A
resolved place becomes a centre and a radius taken from the place's extent, between 1 km and
500 km. Search freezes them into the predicate, so a saved search does not depend on the dataset.

### No Platform Geocoder

`CLGeocoder` is **not used**. Its answers depend on Apple's live service, the OS version and the
device's locale. The same photo would be named differently on an iPhone, an Android phone and the
web, and every lookup would send a coordinate to Apple. The offline geocoder is the only source of
place names on every platform. Until it ships, no client shows place names.

## The Geocoder

Reverse geocoding is an **external project** with its own repository, release cycle and data
pipeline, because its data and quality work is a product of its own and other applications need it
as much as Capsule does ([ADR-0021](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0021-reverse-geocoding-is-an-external-offline-project-and-clgeocoder-is-dropped.md)). Capsule fixes the requirements below. Capsule's tracking issue
carries the full list and the acceptance tests.

- **One core, identical everywhere.** A single Rust core with bindings for Swift and Kotlin
  (UniFFI) and the web (wasm). No platform code decides an answer. A shared golden suite runs
  through every binding and must produce byte-identical output.
- **Deterministic and versioned.** An answer is a pure function of (coordinate, datum, dataset id,
  dataset version, tier, options). No clock, network or randomness is involved, transcendental
  functions come from pure-Rust `libm`, and boundary containment uses exact or fixed-point
  arithmetic. Place ids are stable across releases, and a split or merge publishes a redirect.
- **Boundaries, not nearest city.** Country and administrative areas are resolved by polygon
  containment wherever open boundary data exists. A nearest-populated-place fallback has a stated
  maximum distance and otherwise answers "none". Water resolves to the sea, ocean or lake, never to
  the nearest coastal town.
- **Cell queries.** Besides points, it answers uniform or straddles for a Web Mercator cell
  ([](#geocoded-per-cell-not-per-asset)).
- **Tiered, hash-pinned datasets.** A compact tier bundled with the app (countries, first-level
  areas and localities above a population threshold, a few MiB) and the full tier on demand. Both
  come from the same release with the same ids and names. Where the compact tier's simplified
  boundaries could disagree with the full tier, it answers at the common ancestor instead. Every
  pack is self-describing: magic, format version, dataset id, release, tier, sources and licences,
  length and SHA-256. Capsule pins (release, tier) → SHA-256 in its build and refuses any other
  pack.
- **Distribution that survives.** Packs are content-addressed static files under a signed index,
  so any HTTP server, mirror, peer device or USB stick can serve them, and integrity comes from the
  signature and hashes, never from the host or TLS. The distribution does not depend on GitHub:
  there are mirrors in more than one jurisdiction, and a user can configure their own. Downloads
  are resumable, updates ship as deltas between releases, and a pack can be side-loaded from
  another device, which serves a traveller on a poor connection. Releases come on a regular cadence
  and are reproducible from pinned source snapshots.
- **Private by construction.** A lookup never touches the network. A download never reveals where
  the user is: a tier is fetched whole or as regions the user picks, never in response to a lookup.
- **Disputed territories: de facto authority, neutral label.** Where the dataset marks an area
  disputed, the answer names the locality by its own name and the authority that administers it in
  fact, and never asserts a country. The policy is published with the dataset, and it is part of
  the deterministic answer, so every device gives the same name for the same place.
- **Quality goal: parity with `CLGeocoder` on hard cases.** These are borders and enclaves,
  disputed territories (under the policy above), oceans and lakes, islands, the
  antimeridian and the poles, GCJ-02 input inside mainland China (converted before lookup and
  marked approximate), Hong Kong and Macau, and names in many languages and scripts. Agreement is
  measured per category against a reference set.
- **Licences Capsule can ship.** Code under a licence [](/design/licensing/) permits. Data under
  open licences whose obligations a bundled, app-store-distributed client can meet, with
  attribution carried in the pack and surfaced in the app.
- **Bounded resources.** Packs are memory-mappable, the loader is fuzzed and allocates within a
  bound, and lookups are well under a millisecond on a mid-tier phone for the bundled tier.

## Known Gaps

- No Rust places code exists; the Swift map is a mock and Android has none.
- The geocoordinates release Capsule needs is not yet published.
- The geocoder project does not exist yet; until it ships, no client shows place names.
- The Swift viewer still links the opt-in `CLGeocoder` resolver, which this page retires.
- The budgets are unmeasured targets.

## Validation

- **Grid (unit, property).** Cell assignment is bit-identical on every target, wasm included, for a
  fixture set dense on cell edges, the antimeridian and both Mercator limits.
- **Pyramid (smoke).** After any sequence of imports, edits, hides, deletes, restores and purges,
  the pyramid equals a fresh rebuild; removal restores the sums exactly.
- **Clusters (golden).** A synthetic multi-city library gives the same clusters at three zooms on
  every target; a pan never changes a block it keeps; a viewport across ±180° returns both sides.
- **Scope (unit).** Hidden and trashed assets never appear in clusters, counts or place lists.
- **Place names (golden, smoke).** A per-cell pass over a fixture library matches per-asset lookups
  everywhere except where documented straddles resolve per asset; a crash mid-pass resumes without
  duplicate work; no name reaches a sidecar, a sync payload or a backup.
- **Privacy (smoke).** No places code path opens a network connection.
