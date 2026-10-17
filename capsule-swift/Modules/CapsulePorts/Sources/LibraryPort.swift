import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - LibraryChange

/// A notification that the library's contents moved under the reader.
///
/// Deliberately **not** a diff. A diff computed by the port would have to
/// assume what window the consumer is showing, and would be wrong for every
/// other consumer. Instead the port says *what kind* of thing changed and the
/// consumer re-reads the window it cares about — which is also the only shape
/// that survives a change arriving while the consumer is mid-scroll.
public enum LibraryChange: Sendable, Equatable, Hashable {
    /// Assets were added, removed, or edited. `dayKeys` names the affected
    /// sections when the source can narrow it, so a grid can invalidate two
    /// sections instead of the whole timeline.
    case assetsChanged(dayKeys: Set<DayKey>)
    /// Section sizes changed, so any cached section offsets are stale.
    case dayCountsChanged
    /// The change is too broad to describe. Re-read everything.
    case reload
}

// MARK: - LibraryPort

/// Paged reads over the asset timeline.
///
/// **The most important port in the app.** Every grid, every viewer's
/// neighbour-fetch, and every picker reads through it, so its shape decides
/// whether the app scrolls a hundred-thousand-photo library smoothly or not at
/// all.
///
/// Two rules it exists to enforce:
///
/// - **Reads are windows, never whole arrays.** ``assets(matching:offset:limit:)``
///   returns a ``Page``; there is deliberately no `allAssets()`, because the one
///   call that materialises the library is the one call that will be made from a
///   view body.
/// - **Section sizes come from an aggregate, not from the rows.**
///   ``dayCounts(matching:)`` gives a virtualized grid every section's size in
///   one small read. Without it, a grid must either load every asset to know how
///   tall it is, or guess and then jump — and both are what "virtualized" is
///   supposed to prevent.
public protocol LibraryPort: Sendable {
    /// One window of the timeline, newest first, tie-broken on the asset
    /// identifier so the order is identical on every device.
    ///
    /// Maps to the SDK's `library.query_assets` over the local index.
    func assets(matching query: TimelineQuery, offset: Int, limit: Int) async throws -> Page<LibraryAsset>

    /// Per-day asset counts for the whole query, oldest day first.
    ///
    /// Maps to the SDK's `library.day_histogram`. The aggregate a virtualized
    /// grid sizes its sections and its scrubber from; see
    /// ``Swift/Array/sectionOffsets`` for turning it into row offsets.
    func dayCounts(matching query: TimelineQuery) async throws -> [DayCount]

    /// Total assets matching the query, when the caller needs the number
    /// without the rows.
    ///
    /// Maps to `library.count_assets`.
    func assetCount(matching query: TimelineQuery) async throws -> Int

    /// Resolve one asset, or `nil` if it no longer exists.
    ///
    /// Maps to `library.get_asset`.
    func asset(for id: AssetID) async throws -> LibraryAsset?

    /// Resolve several assets in one round trip, in the order requested.
    /// Missing ids are omitted rather than represented by a placeholder.
    ///
    /// Maps to `library.get_assets`.
    func assets(for ids: [AssetID]) async throws -> [LibraryAsset]

    /// The signed sidecar behind an asset, for the metadata inspector and for
    /// surfacing superseded captions.
    ///
    /// Maps to `sidecar.read`.
    func sidecar(for id: AssetID) async throws -> SidecarV1?

    /// The asset's provenance chain, oldest first — the activity history.
    /// Present even for a purged asset, whose chain survives as a
    /// tombstone-with-history.
    ///
    /// Maps to `provenance.chain`.
    func provenanceChain(for id: AssetID) async throws -> [ProvenanceRecord]

    /// A stream of change notifications for as long as the stream is held.
    ///
    /// Matches the existing `AssetProvider.changes()` pattern in `AssetKit`.
    func changes() -> AsyncStream<LibraryChange>
}
