# ADR-0021 — Reverse geocoding is an external offline project, cached per grid cell; CLGeocoder is dropped

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Places — Place Names](../capsule-docs/src/content/docs/design/places.md#place-names), [Places — The Geocoder](../capsule-docs/src/content/docs/design/places.md#the-geocoder)
- **Issues:** #989, #325, #986

## Context

The only place names in Capsule come from the Apple viewer.
`capsule-swift/Modules/FeatureViewer/Sources/Info/PlaceNameResolver.swift` calls
`CLGeocoder().reverseGeocodeLocation`, opt-in per device, with no cache. Android, the CLI and the
web have no place names.

`CLGeocoder` has three problems for Capsule:

- It sends every looked-up coordinate to Apple.
- Its answer depends on Apple's live service, the OS version and the device's locale, so one photo
  gets different names on different devices.
- It is rate-limited, which makes naming a 100,000-photo library a multi-day trickle.

Issue #989 planned an offline gazetteer from geocoordinates' `reverse` feature (GeoNames
cities1000, nearest populated place, about 5 MB), with `CLGeocoder` kept as an optional Apple
provider.

A nearest-populated-place lookup is wrong at exactly the cases users notice:

- borders, where the nearest town is in the other country;
- water, where it names a coastal town for a photo taken at sea;
- sparse regions.

Doing better takes boundary polygons, multilingual names, a disputed-territory policy, a data build
pipeline, and a distribution channel that survives without GitHub and reaches travellers on poor
connections. That is a data product with its own release cycle, and it is useful far beyond
Capsule.

Prior art: placewise (`amolvenkataraman/placewise`) is an offline Python geocoder. It uses Natural
Earth, GeoNames, Wikidata and national boundary sources, ships checksummed memory-mapped packs from
a separate data repository with mirror support, and has a border and island test set. It is not
shaped for Capsule:

- it is Python, with no Rust core or mobile bindings;
- it has English names only;
- its packs are distributed as GitHub release assets;
- its "auto" download mode tells the host which country was looked up.

It does demonstrate the pack, mirror and quality approach.

## Decision

Reverse geocoding is a separate project, in its own organization and repository. Capsule specifies
its requirements:

- one Rust core with Swift, Kotlin and wasm bindings;
- deterministic, versioned answers;
- boundary containment;
- tiered, hash-pinned datasets;
- distribution that is signed, mirrorable and independent of GitHub;
- no network during lookup;
- parity with `CLGeocoder` on hard cases;
- a published disputed-territory policy of de facto authority with a neutral label: a disputed
  area is named by its locality's own name and the authority that administers it in fact, never
  by an asserted country, identically on every device.

Capsule geocodes **per level-16 Web Mercator cell** and descends to finer cells, then to the
individual asset, only where the geocoder reports that a boundary crosses the cell. Names are
cached in the catalog, keyed by dataset version and language. They are never written to sidecars,
never synced and never backed up.

`CLGeocoder` is dropped entirely, not kept as an option, so that every device shows the same name
for the same photo.

## Consequences

- Until the geocoder ships, no client shows place names, including iOS, which loses its opt-in
  `CLGeocoder` names. The viewer's resolver and the `app.settings.security.place_names.*` keys are
  removed.
- Capsule's dependency is a pinned SDK and pinned dataset hashes. A dataset update is a deliberate
  version bump that lazily re-resolves cached names.
- geocoordinates stays the dependency for coordinate math (Web Mercator, tiles, bounds, geodesy,
  GCJ-02). Whether the geocoder absorbs geocoordinates' `reverse` feature is that project's call.
- The geocoder's data licences need a licensing review (attribution and share-alike terms) before
  any dataset is bundled.
- The quality comparison against `CLGeocoder` needs Apple devices and a human review of the
  disagreements.
