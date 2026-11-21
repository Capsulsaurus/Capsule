# ADR-0022 — Android maps use the Google Maps SDK first, behind a renderer port; Rust computes the clusters

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Places — Clusters per Zoom](../capsule-docs/src/content/docs/design/places.md#clusters-per-zoom), [Clients — Map Renderer Port](../capsule-docs/src/content/docs/design/clients.md#map-renderer-port)
- **Issues:** #988

## Context

The Places map exists only on Apple, as a SwiftUI `Map` over a mock `PlacesPort`. Its clustering
granularity is fixed at 6 (`capsule-swift/Modules/FeatureCollections/Sources/PlacesMapView.swift:177`).
Android has no map. Nothing in Rust clusters.

Issue #988 planned Rust clustering over geocoordinates and an additive server endpoint returning a
GeoJSON cluster layer for the web. A server endpoint cannot work: coordinates are encrypted metadata,
and the server never holds them.

Android renderers considered:

- **Google Maps SDK for Android.** It is the basemap Android users know, has mature gestures and
  accessibility, and needs no tile hosting. It requires Google Play services and an API key, and
  Google learns each viewport.
- **MapLibre Native.** It is open source and works with self-hosted or offline vector tiles, which
  keeps viewports private. It needs a tile source Capsule would have to host or choose, plus a
  style, before it matches the Google basemap's quality.
- **osmdroid.** It is raster only, the UI looks dated, and the project is in maintenance mode.

## Decision

- Rust (`capsule-core::places`) computes clusters per zoom level from a persisted cell pyramid in
  the catalog, and every client only draws.
- Android v1 draws with the Google Maps SDK through the map renderer port
  ([Clients — Map Renderer Port](../capsule-docs/src/content/docs/design/clients.md#map-renderer-port)),
  which keeps each renderer's viewport and datum conventions out of the core.
- MapLibre is the first post-v1 renderer behind the same port.
- The web clusters in wasm.
- Maps are on by default. A per-device *Show maps* setting starts on, and the first time a map
  would load, a one-time in-context notice names the tile provider, says tiles reveal the viewed
  area and zoom but never a photo's coordinates, a count or a place name, and links to turning
  maps off. With maps off, no tile is fetched and the place list replaces the map.

## Consequences

- All clients show identical clusters for identical synced state. No renderer's clustering
  (Google's utility library, MapKit's annotation clustering) is used.
- The basemap provider learns the viewport, never a photo's coordinates. A map is the expected
  default in a photo library, so maps stay on and the disclosure is made once per device, before
  the first tile, rather than gating the feature. MapLibre with self-hosted or offline tiles
  removes even that.
- An Android device without Play services gets the place list instead of a map until MapLibre
  lands. A de-Googled device is therefore a known gap for v1.
- Android needs a Maps API key restricted to the app's signing certificate. Builds without one fall
  back to the place list.
- There is no server places endpoint, ever.
