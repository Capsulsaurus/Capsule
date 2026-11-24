---
title: Clients
description: Platform tiers and design language, what every client must validate, the client standards every client is built to, and the sandboxed decoder
audit: none
---

Clients are native per platform. Cross-platform logic (`verify_asset` per [](/design/cryptography/keys/#write-authorization), the [](/design/import/pipeline/), the library layout of [](/design/filesystem/client/)) lives in `capsule-core`, reached through `capsule-sdk`; a client owns rendering, input and platform integration. This doc owns what every client MUST do, since skipping it makes the client _faulty_ ([](/design/threat-model/#client-class-taxonomy)), the standards every client is built to, and the sandboxed decoder. [](/design/local-gallery/) owns the offline-first requirements.

## Design Priorities

- **Native** per platform, for familiar usability and platform optimisation: SwiftUI on Apple platforms and Jetpack Compose on Android, each over the shared Rust core through its UniFFI bindings ([ADR-0005](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0005-one-uniffi-namespace-pair-reaches-the-apps.md)). No cross-platform UI framework sits between Rust and a client.
- **Minimal divergence.** Heavy logic is centralised in `capsule-core` / `capsule-sdk`, so client code is mostly display.

### Platform Tiers

Every native client is meant to end up fully featured and platform-integrated; tiers are an ordering, not a hierarchy.

| Tier                | Platforms              | Meaning                                     |
| ------------------- | ---------------------- | ------------------------------------------- |
| 1 — stabilizing now | iOS, Android (phone)   | Every feature lands here first.             |
| 2 — next            | iPadOS, Android tablet | Large-screen adaptations of tier 1.         |
| 3 — after           | macOS                  | Shares its views with iPadOS.               |
| Deferred            | Windows, Linux         | Real targets, not started.                  |
| Not on the ladder   | Web                    | Permanently read-only; see [](#web-client). |

### What Clients Share

- **iPadOS and macOS share views**; divergence is confined to the shell (menu bar, Settings scene, windows).
- **iOS and Android share layouts and feature sets, not design language** ([](#platform-parity)).
- **Apple logic is shared** across iOS, iPadOS and macOS: one route enum, one router, and shells chosen by size class, not `#if os(...)`. Android phone and tablet likewise.

A difference between two clients MUST be justified by a difference in the platform (screen, input, convention), never by which was written first.

### Design Language

The skeleton (which screens exist and how they nest) is shared and the material is per-platform: HIG, stock controls and SF Symbols on Apple, Material on Android. Neither emulates the other, and there is no third house style. Bespoke components exist only for concepts without a platform analogue (seal state, verification verdicts, quarantine, the degrade ladder, the smart-album predicate builder, ceremony progress), drawn in each platform's idiom. Strings are keyed by surface, not client ([](/design/i18n/)), and layout is direction-agnostic and expansion-tolerant on every client ([](/design/i18n/#layout)). [](/development/visual-language/) owns the one brand token a native client adopts, its accent.

### Responsiveness

Capsule MUST feel at least as fast as the platform's own photo app (Photos on Apple, the Gallery or Google Photos app on Android) on the same device, at every library size.

- **Instant on press.** Every input produces a visible response in the next frame (≤16 ms at 60 Hz): a pressed state, the destination screen, or an in-place change. Nothing waits for a spinner first.
- **Local first.** Navigation and edits (labels, albums, favourites, time and location fixes) apply optimistically from local state within 100 ms. The network write follows and never blocks the screen, and a rejected write rolls back visibly with the reason.
- **No press waits on the network.** Work that needs the server (restore, share, sync) starts immediately with visible progress and can be left; the UI never blocks on a request.
- **The main thread is for drawing.** Decoding, hashing, crypto and database queries run off the UI thread, and a frame never waits on them.
- **Measured, not asserted.** A parity benchmark runs each tier-1 client and the platform photo app on a 100,000-asset library on the same physical device. It measures cold start to first thumbnail, scroll frame rate and dropped frames, full-resolution open time, and input-to-frame latency.
  - Both apps are timed by the same harness calls, so its overhead cancels out. Input-to-frame latency is the exception: a harness outside the app cannot see its frames, so the client marks each tap itself, from delivery until the answering frame is drawn. The platform app marks nothing, so that measure is reported as not compared.
  - Each device has a recorded baseline of the client's own numbers. A regression past it fails the run, and a baselined measure that stops being reported counts as a regression. Parity with the platform app is reported beside it rather than gating, because the platform app changes under the device's own updates while the baseline changes only when re-recorded.
  - Results recorded on another device, platform or library size are refused, not compared. Emulators and simulators never stand in for a bench device.
  - The harnesses are Macrobenchmark on Android and XCTest metrics on Apple, both driven by `mise run bench-apps`. They need a prepared device, so the run is a manual workflow on self-hosted bench runners (`bench-apps.yml`), not a gate on every pull request. In production the Android client logs JankStats frame summaries, so the field reports the same jank signal the bench measures.

The TUI follows the same frame bound for input response ([](/development/testing/)).

### Web Client

**Permanently read-only**: authenticated viewing, public share links ([](/design/share-links/)) and guest upload links ([](/design/web-upload/)). It never writes library state or enrolls a device, because a browser holds neither hardware-bound device keys nor write-tier keys ([](/design/cryptography/keys/)). Guest drops are keyless and sealed to the recipient, so they need no write authority; [](/design/web-upload/) owns that path.

A web session opens through either door the server offers: a password or, where `server-info` publishes `auth.oidc`, the identity provider's authorization-code flow. The browser binds itself to that ceremony with an S256 `binding_challenge` whose secret stays in the tab's `sessionStorage` until the provider redirects back to `/login/oidc-callback`, which the deployment registers as its `OIDC_REDIRECT_URL`. Either door ends in the same session and second-factor step, and neither grants anything a read-only client lacks.

**Trust bound.** The web client is code the server serves. Every guarantee it gives a viewer or a guest holds only while that server serves honest code: that a share link's fragment, a passphrase or a drop's plaintext never reaches the server. A native client keeps those guarantees against a hostile server. The web client cannot, and the share-link and guest-drop surfaces state their guarantees with this bound ([](/design/share-links/), [](/design/web-upload/)).

It is **server-owned** (shipped and updated with the server) and **version-locked** to it, the one client allowed to assume its peer's exact version. Native clients MUST stay lenient across server versions, which is why protocol evolution is additive ([](/design/versioning/)).

A **progressive web app is out of scope, permanently**: no web app manifest, service worker, offline mode or install prompt. Offline and installed use is what the native clients are for, and a cached web client would outlive its server's version, breaking the version lock.

## Platform Limitations

Some features are platform-limited, notably auto sync ([](/design/sync/#auto-syncing)). Background upload and OS-scheduled sync are post-v1, so v1 sync is foreground-initiated. When background upload lands on iOS, the core still builds every request and the OS only ships sealed files ([](/design/sync/#background-execution)). Server-driven wake is post-v1 and never uses APNs/FCM ([](/design/notifications/#tier-1--wake)). Local, pre-armed alerts are v1 ([](/design/notifications/)).

## Client Standards

The standard every Capsule client is built to. Each standard has a core half, in Rust and exported over UniFFI (and wasm where the web client shows it), and a native half that only renders. A client that reimplements a core half diverges by construction, which [](#what-clients-share) rules out.

### Native Shell Duties

- **Report the path.** Feed the OS path, power and thermal listeners to the session ([](/design/networking/#path-snapshot)). This input is what keeps a phone from uploading originals over a metered network.
- **Render admission prompts.** Show every `ApprovalRequired` event as the [](#metered-prompt).
- **Send no request of its own to a Capsule server.** Every Capsule operation leaves through the SDK ([](/design/networking/#one-http-stack)). A native HTTP stack may run only request specs the core built, for an OS transfer daemon ([](/design/sync/#background-execution)).
- **Never state the protocol date** ([](/design/versioning/#protocol-version-source)).

### Asset Info Page

Every client's info page for an asset (the CLI's `capsule show`, the Apple info panel, the Android info sheet and the web viewer's details) renders **one projection the core computes**. No client reads the sidecar or parses EXIF itself.

- **Rows, in order.** The projection returns the page's sections and rows. Each row has a stable key, a typed value and a state.
  1. **Caption**, editable where the client writes.
  2. **Capture.** The effective capture time with its zone, and the as-imported time when a correction overrides it.
  3. **Camera.** Camera model, lens, aperture, shutter speed, ISO and focal length; for video, codec, frame rate and duration; then dimensions, file type, the original file name and the original's size.
  4. **Location.** The effective fix, with its datum when it is not WGS-84, and the place name ([](/design/places/)). The as-imported fix when an edit overrides it. A map preview through the [](#map-renderer-port).
  5. **Organization.** Rating, labels, AI tags, albums, stack placement, cull flag, hidden, and whether the asset is in trash.
  6. **Library.** Import time, availability (original on this device, awaiting original, or on the server only), the plaintext content hash, and the number of provenance records.
- **No row is hidden for being empty.** An absent value is a fact the user may have come to check. A field the user can set and has not reads _not set_. A field the source never recorded reads _not recorded_. Both strings are catalog keys of the row ([](/design/i18n/#catalogs)).
- **Typed values, platform formatting.** The projection carries instants, rationals (shutter speed), numbers and coordinates, never formatted strings. Each platform formats them for the user's region ([](/design/i18n/#language-and-region)).
- **The sidecar alone.** The projection reads only the sidecar and the catalog, so the page is complete offline with only a thumbnail on the device ([](/design/local-gallery/)). The camera rows therefore come from fields the sidecar records at import, never from parsing an original the device may not hold: `camera_id` and `capture_settings` ([](/design/metadata/#capture-settings)), `video` ([](/design/metadata/#video-info)) and `original_filename` ([](/design/metadata/#original-file-name)).

### Developer Live Metrics

Every client has a hidden **Developer → Live metrics** page. It is reached by a deliberate gesture on the About screen's build row, the platform's own developer-options convention, and it appears in no menu. The CLI and the TUI expose the same data as a `--json` subcommand.

- **What it shows.** The core's diagnostics snapshot, merged with what only native code can measure. [](/development/performance/#typed-stage-events-and-the-diagnostics-snapshot) defines every metric. Beside the metrics, the page shows the current path snapshot and its generation, the work admission is holding, and the protocol window each server advertised.
- **What it costs.** It refreshes about once a second while visible, and pulls nothing while closed.
- **Where it goes.** Nowhere ([](/development/performance/#on-device-metrics-no-telemetry)). The user may share a text export of the page themselves, through the platform share sheet.

### Map Renderer Port

Maps are computed in Rust and only drawn natively. The core answers each viewport with that zoom level's clusters; [](/design/places/) owns the clustering. A tap resolves through the core, either to a zoom that splits the cluster or to the assets it holds.

- **The viewport is renderer-neutral.** The renderer reports its viewport as Web Mercator (EPSG:3857) bounds, a zoom on the 256 px tile basis (zoom 0 is the world in one 256 px tile) and the screen scale. A renderer whose own zoom uses another basis (MapKit's span, a 512 px vector-tile zoom) converts before it reports, so no renderer's convention reaches the core.
- **The renderer declares its basemap datum**, `wgs84` or `gcj02`, when it attaches and again whenever its basemap changes. The core positions every cluster and pin in that datum.
- **What the core returns.** Each cluster carries an id, a coordinate in the declared datum, a count, a representative asset and an `approximate` flag. A pin for a single asset carries the same flag. `approximate` is set when the position rests on an inexact datum conversion ([](/design/places/#datum-and-basemaps)), and the renderer draws such a point visibly as approximate.

| Platform | Renderer                                                       |
| -------- | -------------------------------------------------------------- |
| Apple    | MapKit                                                         |
| Android  | Google Maps SDK in v1; MapLibre after v1, behind the same port |
| Web      | A web map renderer, fed clusters by `capsule-wasm`             |

- **Clusters only.** The renderer receives the viewport's clusters, never the library's coordinates in bulk, and sends no coordinate beyond the tile requests its viewport needs. Tile requests still tell the map provider which regions a user looks at. [](/design/places/) owns that disclosure and the setting that turns maps off.
- **Replaceable.** A renderer is swapped without touching the core, and a fake renderer drives the port in the core's tests.

### Metered Prompt

The native rendering of `ApprovalRequired` ([](/design/networking/#admission-and-priority)): a standard alert on Apple and a Material dialog on Android, each in its platform's idiom.

- **It says what and how much.** The prompt names the work and its estimated size, formatted by the platform's byte formatter: "Download 3 originals (412 MB) on this metered network?". When the OS data saver is on, the prompt says so, because the OS asked the user's apps to save data.
- **Three answers.** _Download_ (this batch), _Always on this kind of network_ (changes the user's policy for that class on metered or constrained paths) and _Not now_ (holds the batch).
- **One at a time.** There is one prompt per user action. Prompts never stack and never come from the background. Held work shows as waiting for Wi-Fi in the transfer surface, where the user looks for it.
- **Reversible.** A network settings page lists every class with its allow, ask or deny on metered and on constrained paths, so the user can undo an _Always_ in a place they can see it.

### Platform Parity

**A feature on one phone and not the other is a plan bug.** A feature lands in the CLI first ([](/development/testing/)), then on iOS and Android in the same plan. A plan that schedules one phone files the other phone's work with it. A justified divergence is recorded on the feature's issue, with the platform reason that justifies it ([](#what-clients-share)).

Each feature is checked against this list on both phones:

- [ ] Its logic is in the core and exported, not reimplemented natively.
- [ ] Its screens and their nesting match the shared skeleton ([](#design-language)).
- [ ] It uses the same string keys and handles the same `error.*` codes.
- [ ] Its offline and empty states match ([](/design/local-gallery/)).
- [ ] Any asset facts it shows come from the [](#asset-info-page) projection.
- [ ] Its transfers are admitted by class, and its prompts are the [](#metered-prompt).
- [ ] Its OS integrations have a counterpart on the other platform ([](/design/platform-integrations/)).
- [ ] Accessibility labels, text scaling and right-to-left layout are covered.
- [ ] It is tested at the same layers on both ([](/development/testing/#by-layer)), and the responsiveness benchmark covers its screens ([](#responsiveness)).

## Client Validation Duties

Clients MUST refuse to apply state they cannot validate. The full checklist is [](/design/threat-model/validation/#client-side-validation-invariants); in summary:

- **Run `verify_asset`** ([](/design/cryptography/keys/#write-authorization)) on every received manifest, on every receiving path (sync, federation, peering, restore). Quarantine on failure; never silently drop or accept.
- **Refuse forward-version writes**: any `sidecar_schema`, `crypto_suite_id` or `protocol_version` above the client's max.
- **Enforce the handshake**: send `X-Capsule-Protocol` on every request; on `426` stop, never downgrade.
- **Check the provenance chain**: refuse a manifest whose `prior_provenance_hash` is behind the local `latest_provenance_hash` ([](/design/sync/#stale-revival-detection)).
- **Reject unknown values of every closed enum** as structural errors ([](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar)).
- **Preserve unknown CBOR keys** within a known schema, but never act on them.
- **Decode remote-origin bytes only in the [](#sandboxed-decoder).**
- **Honour the forbidden behaviours** ([](/design/threat-model/schema-rules/#forbidden-client-behaviors)): backdating, stripping unknown fields, overwriting provenance, signing for an unheld epoch, `revoke_all_sessions` without master-key proof.
- **Run the recovery verification cadence** ([](/design/backup-recovery/#recovery-verification-cadence)) and never persist the passphrase or anything that can satisfy the check.

## Reading State From a Newer Client

Tolerant reads, fail-closed writes ([](/design/principles/#postels-law-asymmetric)):

- **Render the known, surface the unknown.** Show every understood field plus a non-destructive "created with a newer version" indicator; do not fail, hide or quarantine. A higher `sidecar_schema` is readable only under explicit read-only opt-in.
- **Never strip or rewrite** a structure the client cannot fully represent; editing is disabled behind the indicator.
- **Writes still fail closed** at the handshake ([](/design/threat-model/validation/#protocol-and-capability-negotiation)).

## Sandboxed Decoder

The contract is fixed. Platform implementations are post-v1, and until then in-process decoding of remote-origin bytes is a documented deviation.

The server never holds plaintext, so decoding, the client's largest attack surface, happens on the client. Invariants:

- Every remote-origin asset is decoded in a separate process or WASM sandbox with **no filesystem write, no network and no shared memory** with the host: XPC service / extension on Apple, `isolatedProcess` on Android, a privilege-dropped subprocess on desktop, Worker WASM in the browser. A platform that cannot meet one documents the deviation.
- IPC carries only the pixel buffer or an error code.
- **The sandbox may crash.** The host logs it, reports a decode failure and restarts the sandbox on the next request.
- Local-origin bytes may bypass the sandbox by user option; the default is to sandbox everything.
- After a small retry budget (default 3), the asset is shown as "unreadable on this device" and **kept**, never removed ([](/design/filesystem/maintenance/#repair)).
- The v1 deviation covers every remote-origin source, guest drops included: a drop a stranger uploaded ([](/design/web-upload/)) is decoded in process when its owner reviews it.

## Known Gaps

- **No info projection.** `capsule show` builds its own view from the sidecar (`capsule-cli/src/show.rs`). The Apple panel parses EXIF from the original in Swift and reads the filename from PhotoKit. The Android detail screen is a placeholder, and the web client has none. The sidecar implementation has neither `capture_settings` nor `video`.
- **No live metrics page** on any client.
- **No map port.** The Apple info panel draws MapKit directly, with no clusters from the core.
- **No metered prompt, and no path reports** ([](/design/networking/#known-gaps)).

## Test and Performance Tooling

Per-platform pins; library pins carry their rationale beside the pin in each manifest.

- **Apple:** swift-testing is the sole unit/smoke framework, with XCTest only in XCUITest bundles. Profiling uses Instruments and field metrics MetricKit, with no third-party on-device APM.
- **Kotlin/Android:** JUnit 5.
- **Web:** bun's built-in runner.

## Validation

- **`verify_asset` chokepoint (unit).** Every receiver path routes through the one implementation.
- **Forward-version (unit).** Versions above max refuse writes; unknown keys and an opt-in higher schema render with the indicator, editing disabled, nothing stripped.
- **Sandbox crash and boundary (smoke, per platform).** A CVE corpus never kills the host; the asset becomes "unreadable on this device"; the sandbox cannot read host files, open sockets or write outside scratch.
- **Info projection (unit).** Every row reports its state for a set, an unset and a never-recorded field. The CLI's `--json` output and the FFI return the same rows in the same order, from a sidecar with no original present.
- **Map port (unit).** A fake renderer's viewport yields the core's clusters, and a tap on a cluster resolves through the core.
- **Metered prompt (smoke, per platform).** A batch of originals on a metered path raises one prompt, and _Not now_ holds the whole batch.
- **Forbidden-behaviour tripwire (unit).** Each forbidden behaviour reachable through a `capsule-core` API returns a structural error; auth-surface items are tested server-side.
