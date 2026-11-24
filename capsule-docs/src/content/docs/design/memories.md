---
title: Memories
description: On This Day, trips, people and birthday memories generated on device from the local index, and the seen and saved state that syncs
audit: none
---

A **memory** is a titled, ordered set of assets the app offers without being asked: the photos from this day in past years, a trip, a person over the years, a birthday. Memories are generated on the client from the local index ([](/design/local-gallery/)); the server never sees one and cannot tell that one exists. This page owns the generators, their determinism and the state that syncs. Widgets that show a memory are owned by [](/design/platform-integrations/#widgets).

**Status.** Planned. No generator exists. Generation belongs in `capsule-library::memories`.

## Generators

Each generator is a pure function of `(local index, today's local date, library settings)`. Given the same inputs, every device produces the same memories with the same ids, so a memory seen on the phone is already seen on the laptop.

| Generator | Members | Shown |
| --- | --- | --- |
| **On This Day** | Assets whose effective capture date ([](/design/metadata/#as-imported-and-user-values)) has today's month and day in an earlier year, at least one asset in at least one earlier year | From the day itself for 7 days |
| **Trip** | A run of consecutive days whose assets sit more than 100 km from the user's home cluster, with at least 20 assets; split by the place clusters ([](/design/places/)) | On the trip's anniversaries, and once 30 days after it ends |
| **Person** | A named person's assets ([](/design/people/)), sampled evenly across the years they appear in | At most once per person per quarter |
| **Birthday** | A named person with a `birth_date`: their assets spread evenly across years | From 3 days before the birthday through the day |

- **Ids.** A memory's id is UUIDv5 of `(generator, key)`, where the key is the date, trip span or `person_id`. The id never reveals a creation time, and two devices derive the same id.
- **Catch-up.** A generator evaluates every day since it last ran, so a week without opening the app still produces that week's On This Day memories.
- **Selection.** Within a memory, assets are ordered by capture time. Rejected assets ([](/design/organization/#culling)) and non-primary stack members are left out; picks and high ratings are preferred when the set is trimmed to its size cap of 60.
- **Home.** The home cluster is the place cluster holding the most assets over the last 365 days. A user-set home overrides it.

## Exclusions

- Hidden and trashed assets are never members, and a memory never reveals that one exists ([](/design/organization/#hidden-assets)).
- A hidden person ([](/design/people/#corrections)) never gets a person or birthday memory.
- A user may exclude a person, a place or a date range from all memories. Exclusions apply before generation.

## Synced State

The library-settings document ([](/design/metadata/#the-library-settings-document)) holds the `memories` section, LWW per memory id:

```rust
MemoryState {
  seen:      bool,
  dismissed: bool,                  // never shown again
  saved:     Option<Vec<asset_id>>, // a snapshot the user kept
}
```

A **saved** memory is a snapshot: it keeps its members even if a later generator run would choose differently, and it is never regenerated away. Exclusions and the home override live in the same section.

## Failure Modes

| Failure | Detected / contained by | Recovery |
| --- | --- | --- |
| Two devices generate on different dates (time zones) | Ids derive from the local date and key, not the device | Each device shows the memories of its own date; seen state merges by id |
| A capture-time correction moves an asset out of a memory | Generation reads the effective time | The next run drops it; a saved snapshot keeps it |
| A newly hidden asset sits in a saved memory | Hidden filter at render time | Never rendered outside the Hidden view |

## Known Gaps

- Nothing in this page is implemented: `capsule-library::memories` does not exist, and the memories section waits on the library-settings document, which is deferred post-v1 ([](/design/metadata/#the-library-settings-document)).
- Person and birthday memories wait on people ([](/design/people/)); trips wait on the place clusters ([](/design/places/)).

## Validation

Tiers per [](/design/principles/#validation-tiers).

- **Determinism (unit).** The same index and date give the same memories and ids on every run and in any asset order.
- **Catch-up (unit).** A run after a gap of *n* days produces every On This Day memory those days would have.
- **Exclusions (unit).** Hidden and trashed assets, hidden people and user exclusions never appear.
- **Saved snapshot (unit).** A saved memory keeps its members after a regeneration that would change them.
