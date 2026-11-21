---
title: Platform Integrations
description: How the native apps sit inside iOS and Android, from the Photos-parity layout and the Shortcuts actions to share targets, widgets, Spotlight and backup on capture, without letting decrypted content leave the app
status: draft
---

This doc owns how each native client plugs into its operating system: the iPhone layout contract against Apple Photos, App Intents and Shortcuts, Spotlight, widgets and the share extension on iOS, and the intents, picker, shortcuts, widgets and capture detection on Android. It also decides what ships in v1 on each platform. It defers platform tiers, design language and validation duties to [](/design/clients/), background transfer to [](/design/sync/#background-execution), offline behaviour to [](/design/local-gallery/), and export stripping to [](/design/metadata/#privacy-on-export).

Each integration is a thin adapter: it resolves to a route the app already has (`capsule://` on Apple, a navigation destination on Android) or to an FFI call the app already makes. It adds no logic of its own, so it is tested through the same ports as the in-app flow.

## The Plaintext Boundary

Capsule's library is end-to-end encrypted, and the operating system is outside that boundary. Every integration obeys these rules.

- **Decrypted content stays in-process.** Pixels, captions, place names, album titles, people and labels are decrypted only inside Capsule's own processes: the app and, where named below, an extension that shares its sandbox. An OS service (the Spotlight index, the widget renderer, Siri and Apple Intelligence, Shortcuts, Android's MediaProvider, AppSearch or a launcher) is outside the boundary.
- **Crossing it is an export.** Content crosses only when the user asks for it to: the share sheet, a Shortcuts action that returns files, or an opt-in index. Every crossing applies the export privacy strip ([](/design/metadata/#privacy-on-export)) and is logged.
- **Opt-in, off by default, revocable.** Any integration that hands the OS library content, including entity names shown in the Shortcuts editor, sits behind a per-library setting that defaults to off. Turning it off deletes what was handed over (the Spotlight index, cached widget timelines) and makes every entity query return nothing.
- **Inbound is always allowed.** Receiving files from another app (a share target, "Save to Capsule") reveals nothing about the library and needs no opt-in, as long as it lands in the default album and lists no album names.
- **No OS cloud backup of the library.** The local index holds decrypted metadata, captions and coordinates among it (`capsule-library/src/db/schema.rs:63-66`). The library root MUST be excluded from iCloud device backup (`isExcludedFromBackup` on the root). On Android it MUST be excluded from Auto Backup and device-to-device transfer on every supported API level (`minSdk` is 26, `capsule-android/build.gradle.kts:26`), so the manifest MUST declare both rule files, each excluding every domain, which covers the library and the catalog:
  - `android:fullBackupContent`, which governs both cloud backup and device transfer on API 26–30, where `dataExtractionRules` is ignored;
  - `android:dataExtractionRules`, with its `<cloud-backup>` and `<device-transfer>` sections, which governs API 31 and later.

  `android:allowBackup="false"` alone is not enough: on API 31 and later it stops cloud backup but not device-to-device transfer. Capsule's own backup is the recovery path ([](/design/backup-recovery/)).

## iOS: Apple Photos Parity

Capsule on iPhone MUST be at least as good to use as Apple Photos. It mirrors Photos' layout and ergonomics, and departs only where one of Capsule's capabilities needs room: provenance, end-to-end-encrypted sharing state, federation, culling, custody, or offline state. A departure names its capability in the table below; one that cannot is a bug. Clients run on iOS 26 and later (`capsule-swift/Project.swift:16`), so the reference is the iOS 26 Photos app. iOS 27 kept that design ([9to5Mac](https://9to5mac.com/2026/06/16/heres-everything-new-for-apple-photos-in-ios-27/)).

### The Reference: Photos on iOS 26

- **Two tabs and Search.** "The Photos app is updated to feature separate tabs for Library and Collections views" ([Apple Newsroom](https://www.apple.com/newsroom/2025/06/apple-elevates-the-iphone-experience-with-ios-26/)). Search is at the bottom right ([Apple Support](https://support.apple.com/guide/iphone/iph7d24753a5/26/ios/26)).
- **Library.** One grid with Years, Months and All at the bottom. Zoom is a pinch or the Sort and Filter button's View Options ([Apple Support](https://support.apple.com/guide/iphone/iph7d24753a5/26/ios/26)). Sort is by Recently Added or Date Captured. Filters include Favorites, Videos, Photos, Edited and Not in an Album, and View Options toggles Screenshots and Shared with You ([Apple Support](https://support.apple.com/guide/iphone/iph2e66e2f2c/26/ios/26)). Select and the filter button sit in the top bar ([MacRumors](https://www.macrumors.com/guide/ios-26-photos-app/)).
- **Collections.** A customizable list: a Layout and Reorder control, and collections pinned under a Pinned heading at the top ([Apple Support](https://support.apple.com/guide/iphone/iph4f36c4148/26/ios/26)). Pinned defaults are Favorites, Recently Saved, Map, Videos and Screenshots, and the sections include Albums, Media Types and Utilities (Recently Deleted, Hidden) ([Apple Support](https://support.apple.com/121870)). Recent Days, People & Pets, Memories, Trips and Featured Photos follow ([MacRumors](https://www.macrumors.com/guide/ios-26-photos-app/)).
- **iPad and Mac.** A sidebar with Library and Collections and the same pinning ([Apple Support, iPad](https://support.apple.com/guide/ipad/browse-your-photo-library-ipad99c6ed03/26/ipados/26)). The Mac adds a Sharing section and Imports under Utilities ([Apple Support, Mac](https://support.apple.com/guide/photos/phtf6b8c37c3/mac)).
- **Locked utilities.** Hidden and Recently Deleted require Face ID, which Capsule already matches (SR1 in [](/design/local-gallery/#security-requirements)).

### Layout Contract

The section table is declared once in `SidebarCatalog.swift` (`capsule-swift/Modules/CapsuleNavigation/Sources/SidebarCatalog.swift`). Today it puts four sections on the phone's tab bar: Library, Browse, Search and Settings. The contract below changes that.

- **Tabs: Library, Collections, Search.** Browse is renamed Collections. Search keeps the search tab role it has today (`capsule-swift/App/Sources/Shell/CompactShell.swift:28`).
- **Settings leaves the tab bar.** Photos has no settings tab because its settings live in the system Settings app. Capsule's cannot (server, keys, devices, recovery), so Settings is reached from an account button in the Collections toolbar. On iPad and Mac it stays a sidebar row and the Settings scene.
- **Library: Years, Months, All.** The `days` timeline focus leaves the Library switcher and becomes a Recent Days collection, as in Photos. Day headers stay inside All.
- **One sort-and-filter menu.** Sort and the filters Photos offers come first. Capsule's filters follow in one group: cull state, custody state (on this device only, sealed, in custody), available offline, and shared or federated.

| Photos surface | Capsule surface | Departure, and the capability that justifies it |
| --- | --- | --- |
| Library grid, Years / Months / All | Library tab, same switcher | None. |
| iCloud status line under the grid | Transfer status line under the grid; it opens Transfers | Same placement. Custody: it reports sealed and in-custody counts, not just "synced". |
| Grid badges | None on healthy assets. One badge only for an exception: quarantined, unreadable on this device, or on this device only past the staleness bound | Custody, provenance. Normal state stays invisible. |
| Favorites (heart) | Rating and cull flag | Culling: Capsule has no favourite. A favourite maps to a 5-star rating, as Takeout import already does ([](/design/import/pipeline/#third-party-importers)). The Favorites pinned collection becomes Top Rated. |
| Pinned: Recently Saved, Map, Videos, Screenshots | Pinned: Recent Imports, Places, Videos, Screenshots | None in shape. Places uses [](/design/places/). |
| Albums | Albums: container albums, then smart albums | E2EE sharing: a shared album's tile carries one glyph for its share state (members, link, federated). The glyph appears only on shared albums. |
| Shared Albums (Mac: Sharing) | Sharing: Shares, Drops, followed federated albums | Federation, share links and guest drops ([](/design/federation/), [](/design/share-links/), [](/design/web-upload/)). |
| Utilities: Duplicates, Hidden, Recently Deleted, Imports | Same, plus Quarantine | Provenance: quarantine holds what failed verification ([](/design/clients/#client-validation-duties)). |
| No equivalent | Transfers, Devices, Peers, Storage | Custody, peering. Transfers lives under Utilities; Devices, Peers, Storage and Federation settings move into Settings. |
| Viewer toolbar: Share, Favorite, Info, Edit, Delete | Share, Rate / Cull, Info, Edit, Delete | Info adds provenance, seal and custody state, with the custody receipt one tap away. |
| Select, then an action bar | Same, plus Cull | Culling: Cull opens the keyboard-first pass ([](/design/organization/#culling-ui)). |
| An optimized original downloads on open, with a progress ring | Same, along the render ladder | Offline state: the ladder upgrades in place ([](/design/transfer/download/#render-ladder)). An "Available offline" filter replaces a per-asset badge. |
| Search with suggestions | Search with the text grammar and library-drawn suggestions | Search runs on device over decrypted content ([](/design/search/)). |

### Shortcuts and App Intents

Capsule exposes App Intents that mirror Photos' own Shortcuts actions by name and behaviour, and adopts Apple's photos App Intent schemas (`@AppIntent(schema: .photos.…)`) where one fits, so Siri and Apple Intelligence recognise them ([Apple Developer](https://developer.apple.com/documentation/appintents/app-schema-domain-photos)). Photos' classic actions are listed by [Matthew Cassinelli's catalogue](https://matthewcassinelli.com/action_groups/media/photos); iOS 26 added Rename Album, Search Photos, Create New Memory and Filter Library ([Apple Support](https://support.apple.com/en-gb/125148)); iOS 27 added Favorite Photos, Hide Photos, Auto Enhance Photos, Find Albums, Delete Albums and Open Photo ([Apple Support](https://support.apple.com/en-us/149045)).

Entities:

- **`AssetEntity`**: id, capture date, media type and album id. The display title is the capture date, never the caption. Its file representation is produced only when a downstream action asks for the file, and producing it is an export. An `AppEntity` has a 10 MB size limit, so pixels are never a stored property ([Apple Developer](https://developer.apple.com/documentation/appintents/appentity)).
- **`AlbumEntity`**: id, title, and whether it is a container or smart album.
- **`ImportEntity`**: one import run with its asset ids.

Gate: every intent that returns or lists an entity requires the Shortcuts opt-in ([](#the-plaintext-boundary)). Intents that only open the app, or only receive files into the default album, do not.

| Photos action | Capsule intent | Schema | Gate | Release |
| --- | --- | --- | --- | --- |
| Find Photos | `FindAssetsIntent` (property query: album, date, media type, label, rating, cull, place) | — | Opt-in | v1 |
| Get Latest Photos / Videos / Screenshots / Live Photos / Bursts | `GetLatestAssetsIntent`, one implementation behind five named actions; Bursts are burst stacks, Live Photos are paired stills | — | Opt-in | v1 |
| Get Last Import | `GetLastImportIntent` → `ImportEntity` | — | Opt-in | v1 |
| Search Photos | `SearchIntent(query)`, opens `capsule://search?q=` | `.photos.search` | None | v1 |
| Open Photo | `OpenAssetIntent`, opens `capsule://asset/<id>` | `.photos.openAsset` | Opt-in (the picker lists assets) | v1 |
| (Open Album) | `OpenAlbumIntent` | `.photos.openAlbum` | Opt-in | v1 |
| Save to Photo Album | `SaveToCapsuleIntent(files, album?)`, runs the import pipeline | `.photos.createAssets` | None for the default album; opt-in to pick an album | v1 |
| Delete Photos | `DeleteAssetsIntent`, moves to Recently Deleted after a confirmation, as Photos asks | `.photos.deleteAssets` | Opt-in | v1 |
| Find Albums | `FindAlbumsIntent` | — | Opt-in | v1 |
| Select Photos | `SelectAssetsIntent`, opens Capsule's picker in the foreground | — | Opt-in | Later |
| Create Photo Album | `CreateAlbumIntent`, creates a container album | `.photos.createAlbum` | Opt-in | Later: needs album creation on the CLI first |
| Rename Album | `RenameAlbumIntent` | `.photos.updateAlbum` | Opt-in | Later |
| Delete Albums | `DeleteAlbumIntent` | `.photos.deleteAlbum` | Opt-in | Later |
| Favorite Photos | `RateAssetsIntent` (a favourite is 5 stars) | `.photos.updateAsset` | Opt-in | Later |
| Hide Photos | `HideAssetsIntent` | `.photos.updateAsset` | Opt-in | Later |
| Filter Library | `OpenLibraryIntent(filter)` | — | None | Later |
| Remove from Photo Album | Not offered. Capsule offers Move to Album and Remove Label instead | — | — | Not planned |
| Create New Memory, Auto Enhance Photos | Not offered | — | — | Not planned |

Remove from Photo Album is not offered because a container album is the cryptographic sharing boundary and every asset belongs to exactly one ([](/design/organization/#albums)). Removing an asset would silently move it into another share boundary. Moving is an explicit, signed action.

**App Shortcuts.** An app may ship up to ten App Shortcuts ([Apple HIG](https://developer.apple.com/design/human-interface-guidelines/app-shortcuts)). Capsule's are parameterless and content-free, so they need no opt-in: Search Capsule, Import Photos, Show Transfers, Start Culling and Back Up Now. Apple may use App Shortcut phrases and titles to train its models ([Apple Developer](https://developer.apple.com/documentation/appintents/appshortcutsprovider)), which is acceptable only because they are static strings.

**CLI parity.** Every intent's core call is reachable as a CLI `--json` subcommand first, per the reference-client rule. The CLI has no search and no album-create subcommand today, so those intents wait for them.

### Spotlight

Spotlight indexing is **opt-in and off by default**. An index of captions, places, album titles or people is library content handed to the OS, and `IndexedEntity` content is also "discoverable by Apple Intelligence" ([Apple Developer](https://developer.apple.com/documentation/appintents/indexedentity)).

- Capsule uses a named index with a data-protection class, never the default index, which "doesn't protect data" ([Apple Developer](https://developer.apple.com/documentation/corespotlight/cssearchableindex/default())).
- The opt-in is per field: album titles; captions and labels; place names. Thumbnails are never indexed.
- Turning it off deletes every searchable item before the setting reports off.
- Release: later.

### Widgets

A widget runs in an extension and its rendered views are archived and drawn by the system, including on the Lock Screen, so photo content in a widget crosses the boundary.

- **v1: a status widget.** Sealed, pending and in-custody counts and the last sync time. No pixels, names or places.
- **Later: a photo widget** (a featured photo or a memory, [](/design/memories/)), opt-in by adding it, with its content marked privacy-sensitive so it is redacted while the device is locked. The extension needs the album keys through a shared keychain access group, which widens the key's exposure to a second process and is decided in [](/design/cryptography/keys/) before it ships.

### Share Extension

**v1.** A share extension receives images and videos from other apps. It copies them into a staging inbox inside the App Group container and exits. It holds no keys and opens no library. The app imports the inbox on its next foreground through the ordinary pipeline. The activation rule names images and movies with explicit maximum counts, because a `TRUEPREDICATE` rule is rejected at review ([Apple archive](https://developer.apple.com/library/archive/documentation/General/Conceptual/ExtensibilityPG/ExtensionScenarios.html)). Extension memory limits are "significantly lower" than an app's ([Apple archive](https://developer.apple.com/library/archive/documentation/General/Conceptual/ExtensibilityPG/ExtensionCreation.html)), so it copies by streaming and never decodes.

Outbound sharing already uses `ShareLink` (for example `capsule-swift/Modules/FeatureTimeline/Sources/SelectionActionBar.swift:52`) and is an export.

### Backup on Capture

The system Photos library is a source, never the store.

- **v1, foreground catch-up.** When the user turns on "Back up from Photos", the app asks for full library access. On every foreground it fetches PhotoKit's persistent changes since its last token ([Apple Developer](https://developer.apple.com/documentation/photokit/phphotolibrary/fetchpersistentchanges(since:))) and imports new captures. `PhotoKitProvider` already implements `PHPhotoLibraryChangeObserver` for live changes while the app runs (`capsule-swift/Modules/AssetKit/Sources/PhotoKitProvider.swift:15`), but nothing constructs it yet. Manual import keeps using the out-of-process picker, which needs no permission ([Apple Developer](https://developer.apple.com/documentation/photokit/delivering-an-enhanced-privacy-experience-in-your-photos-app)).
- **Later, in the background.** `BGProcessingTask` runs only when the device is idle ([Apple Developer](https://developer.apple.com/documentation/backgroundtasks/bgprocessingtask)). `BGContinuedProcessingTask` (iOS 26) continues a user-started task in the background with a cancellable Live Activity ([Apple Developer](https://developer.apple.com/documentation/backgroundtasks/bgcontinuedprocessingtask)). That fits a user-started large import better than any idle-time task. Both are post-v1 under [](/design/sync/#background-execution).

## Android: Google Photos Integrations

Android shares iOS's skeleton (Library, Collections, Search, with Settings behind an account button in the top app bar), drawn in Material ([](/design/clients/#what-clients-share)). The current app is not there yet. Its manifest declares only the launcher activity and two alert receivers (`capsule-android/src/main/AndroidManifest.xml:34-55`). Its list and detail screens are still the template's museum browser: `ListScreen` renders `MuseumObject`s, and `DetailScreen` shows artist, medium, department and credits (`capsule-android/src/main/kotlin/com/justin13888/capsule/screens/DetailScreen.kt:109-117`).

Google does not publish Google Photos' manifest, so its exact intent filters are checked on a device ([](#open-questions)). The surfaces below are the platform's, decided on their merits.

| Surface | Decision | Release |
| --- | --- | --- |
| Share target (`ACTION_SEND`, `ACTION_SEND_MULTIPLE`; `image/*`, `video/*`) | An activity copies each `content://` stream into app-private staging at once, because a URI grant ends with the receiving activity, and imports through the pipeline into the default album. Inbound, so no opt-in. | v1 |
| Import picker | `PickVisualMedia` through androidx.activity, which falls back to `ACTION_OPEN_DOCUMENT` where the system picker is missing ([Android Developers](https://developer.android.com/training/data-storage/shared/photopicker)) | v1 |
| Launcher shortcuts | Static and content-free: Search, Import, Transfers, Cull | v1 |
| Status widget (Glance) | The iOS status widget's content, no pixels | v1 |
| `ACTION_VIEW` for images and video | Opens another app's file in Capsule's viewer with an Import action. Nothing enters the library until the user imports it. | Later |
| `ACTION_GET_CONTENT` / `ACTION_PICK` (Capsule as a picker for other apps) | An export: the user picks, the privacy strip applies, and the bytes stream through a pipe from a content provider, never a plaintext temp file | Later |
| Photo widget | As on iOS, opt-in, no content on the lock screen | Later |
| App Actions | Not built. Google retired dozens of built-in intents in 2024, the photo ones among them ([Android Developers](https://developer.android.com/develop/devices/assistant/release-notes)) | Not planned |
| AppFunctions | The successor path for assistant calls. It needs Android 16, and Gemini calls require a developer-preview programme ([Android Developers](https://developer.android.com/ai/appfunctions)). When built, it mirrors the iOS intent set and uses the same opt-in gate. | Later |
| `CloudMediaProvider` (Capsule's library inside the system photo picker) | Not built. See below. | Not planned |

### Why Capsule Is Not a Cloud Media Provider

Two reasons, each sufficient.

- **Eligibility.** "Only apps nominated by OEMs are eligible" ([Android Developers](https://developer.android.com/guide/topics/providers/cloud-media-provider)), and the provider must be guarded by `MANAGE_CLOUD_MEDIA_PROVIDERS`, a permission only the system holds ([Android Developers](https://developer.android.com/reference/android/provider/CloudMediaProvider)). A Play Store app cannot opt itself in.
- **Plaintext.** The picker syncs the provider's media metadata into the system and receives plaintext file descriptors from `onOpenMedia` and `onOpenPreview` (same reference). Serving them would put the library's metadata into MediaProvider permanently, not at the moment of a user's export.

Revisit only if Android opens eligibility and an on-demand, per-pick model becomes possible.

### Backup on Capture

- **v1, foreground catch-up.** With "Back up camera" on, the app holds full `READ_MEDIA_IMAGES` and `READ_MEDIA_VIDEO`. Android 14's partial "Select photos" grant cannot see new captures ([Android Developers](https://developer.android.com/about/versions/14/changes/partial-photo-video-access)). On each foreground the app queries MediaStore for items added since its last recorded generation (`MediaStore.getGeneration`, API 30; by date added on API 26–29, the app's floor) and imports them. This matches the foreground-only v1 in [](/design/clients/#platform-limitations) and [](/design/sync/#background-execution).
- **Later, detection.** A `ContentObserver` fires only while the process lives, so it is not the durable mechanism. WorkManager's content-URI trigger on the MediaStore collections is ([Android Developers](https://developer.android.com/reference/androidx/work/Constraints.Builder)). It fires once, so the worker re-enqueues itself after each run.
- **Later, upload.** A user-started backup runs as a user-initiated data transfer job on Android 14 and later. Automatic backup runs under WorkManager. That is Google's own split for the two cases ([Android Developers](https://developer.android.com/develop/background-work/background-tasks/data-transfer-options)). A `dataSync` foreground service is capped at six hours a day on Android 15 ([Android Developers](https://developer.android.com/develop/background-work/services/fgs/timeout)).
- **Play policy.** Broad photo access is reserved for apps whose core function needs it, declared on a form, and backup apps are not named as qualifying ([Play Console Help](https://support.google.com/googleplay/android-developer/answer/14115180)). Capsule declares itself a gallery app, which it is.

## Release Summary

| | iOS | Android |
| --- | --- | --- |
| v1 | Photos-parity layout; App Intents for find, latest, last import, search, open, save, delete and find albums (opt-in gated); five App Shortcuts; share extension; status widget; foreground capture catch-up; library excluded from OS backup | Parity skeleton replacing the template; share target; photo picker import; launcher shortcuts; status widget; foreground capture catch-up; library excluded from OS backup |
| Later | Select, album create/rename/delete, favourite, hide, filter intents; Spotlight (opt-in); photo widget; background capture via `BGContinuedProcessingTask` and `BGProcessingTask` | `ACTION_VIEW`; Capsule as a picker; photo widget; AppFunctions; content-URI-triggered capture with user-initiated and WorkManager uploads |
| Not planned | Remove from Album, Create Memory, Auto Enhance | App Actions, `CloudMediaProvider` |

## Implementation Status

- **Implemented:** the `capsule://` scheme and its parser (`capsule-swift/Project.swift:480-499`), `ShareLink` exports, MetricKit and signposts on Apple, and JankStats on Android.
- **Planned:** every App Intent, App Shortcut, Spotlight index, widget, share extension and share target above; the layout changes; capture catch-up; OS-backup exclusion. Today the Apple project builds one app target and no extension (`capsule-swift/Project.swift:470`), and the Android manifest has neither `fullBackupContent` nor `dataExtractionRules` while setting `allowBackup="true"` (`capsule-android/src/main/AndroidManifest.xml:11`).

## Open Questions

These need a device or a human. They are tracked as triage issues, not decided here.

- A Photos-parity walkthrough on a current iPhone, checking the layout contract surface by surface.
- Every v1 intent run in the Shortcuts app on iOS 26 and iOS 27, including the opt-in gate's empty-query behaviour.
- Google Photos' intent filters, launcher shortcuts and widgets read off a device (`adb shell dumpsys package`), and Capsule's share target, picker and shortcuts checked on a device.

## Validation

- **Boundary (unit).** With the opt-in off, every entity query returns empty and every gated intent refuses. Turning it off deletes the Spotlight index and widget timelines.
- **Routing (unit).** Every intent and shortcut resolves to an existing route, tested like `capsule-swift/App/Tests/RouteDestinationTests.swift`.
- **Inbound (smoke).** A shared batch lands in staging, imports once, and re-delivering it imports nothing new.
- **Backup exclusion (smoke).** The library root carries the exclusion on iOS, and the Android backup rules exclude every domain.
- **Layout (UI).** The tab set and the section placement match this contract in the existing UI sweep.
