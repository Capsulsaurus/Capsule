import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Scope resolution and streaming scan

/// The half of ``ImportPort`` that serves the import *pipeline* screens, as
/// opposed to the three-call scan → plan → execute spine in `MockImportPort`.
///
/// Split into its own file for the same reason the pipeline is split into its
/// own screens: a picked location, a streaming scan, a retry, and a history are
/// four independent concerns, and interleaving them with the planner would make
/// the planner's invariants harder to read than they already are.
extension MockTransferStore {
    /// Turn a picked location into a scope.
    ///
    /// The scope id is derived from `(platform, source_kind, locator)` in
    /// `capsule-core` and never recomputed in Swift; this stands in for that
    /// derivation with a keyed hash over the same three inputs, so the mock has
    /// the property that matters here — the same pick always yields the same
    /// scope — without pretending to reproduce a value two devices must agree
    /// on byte-for-byte.
    public func resolveScope(sourceKind: SourceKind, locator: String) async throws -> ImportScope {
        let material = "\(PlatformEnvironment.platformTag)/\(sourceKind.rawValue)/\(locator)"
        return ImportScope(
            scopeID: MockHash.hex(Self.fold(material), digits: 16),
            platform: PlatformTag(rawValue: PlatformEnvironment.platformTag),
            sourceKind: sourceKind,
            locator: locator
        )
    }

    /// Enumerate a source, streaming progress.
    ///
    /// Emits a declared total only for the sources that genuinely have one. A
    /// PhotoKit fetch result knows its count before the first item is read; a
    /// directory walk, a mounted volume, and a Takeout archive do not, and a
    /// mock that invented a total for them would let a determinate progress bar
    /// ship without anyone noticing it cannot exist.
    public nonisolated func scanStream(_ scope: ImportScope) -> AsyncStream<ImportScanEvent> {
        AsyncStream { continuation in
            Task {
                let scan = try? await self.scan(scope)
                guard let scan else {
                    continuation.yield(.cancelled(itemsFound: 0))
                    continuation.finish()
                    return
                }
                let total = Self.declaresTotal(scope.sourceKind) ? scan.candidates.count : nil
                continuation.yield(.started(expectedTotal: total))
                var bytes: UInt64 = 0
                for (position, candidate) in scan.candidates.enumerated() {
                    bytes += candidate.byteSize ?? 0
                    continuation.yield(.progress(ImportScanProgress(
                        itemsFound: position + 1,
                        bytesFound: bytes,
                        currentLocator: candidate.locator,
                        expectedTotal: total
                    )))
                }
                continuation.yield(.finished(scan))
                continuation.finish()
            }
        }
    }

    /// A stable hash of the locator material.
    ///
    /// Deliberately **not** `String.hashValue`: Swift seeds that per process, so
    /// a scope id built on it would differ between two launches of the same
    /// build — the one property a scope id is defined by not having.
    private static func fold(_ text: String) -> UInt64 {
        text.utf8.reduce(UInt64(0xCBF2_9CE4_8422_2325)) { accumulated, byte in
            (accumulated ^ UInt64(byte)) &* 0x0000_0100_0000_01B3
        }
    }

    /// Whether the source can state its own size before it is walked.
    private nonisolated static func declaresTotal(_ kind: SourceKind) -> Bool {
        switch kind {
        case .cameraRoll, .screenshots, .appCollection: true
        case .folder, .watchedDirectory, .removableVolume, .takeoutArchive, .unknown: false
        }
    }
}

// MARK: - Retry

public extension MockTransferStore {
    /// Re-attempt the locators a run failed on.
    ///
    /// Most retries succeed, and a fixed minority keep failing — because a UI
    /// that only ever sees a retry work would never exercise the state a user
    /// actually gets stuck in. Which locators keep failing is a function of the
    /// locator, so the second retry of the same file behaves like the first.
    func retry(_ importID: ImportID, locators: [String]) async throws -> [ImportResult] {
        locators.map { locator in
            let hash = MockHash.value(
                seed: configuration.seed,
                index: locator.utf8.count,
                salt: .syncState,
                sub: importID.rawValue.utf8.count
            )
            guard !MockHash.occurs(hash, perMille: 220) else {
                return ImportResult(locator: locator, outcome: .failed(.uploadChecksumMismatch))
            }
            return ImportResult(
                locator: locator,
                outcome: .imported(assetID: MockHash.hex(hash, digits: 12), derivativesDeferred: false)
            )
        }
    }
}

// MARK: - History

extension MockTransferStore {
    /// How many past runs a populated library has.
    private static let historyDepth = 6

    /// Past runs, newest first, dismissed ones omitted.
    ///
    /// Derived rather than accumulated: the app's composition root builds a new
    /// world on every launch, and a history that only contained runs performed
    /// during this session would make the screen permanently empty. An empty
    /// library still has no history — nothing has ever been imported into it,
    /// and inventing runs would contradict the very state that scenario exists
    /// to show.
    public func history(limit: Int) async throws -> [ImportSessionRecord] {
        guard store.library.assetCount > 0, limit > 0 else { return [] }
        let scopes = try await availableScopes()
        guard !scopes.isEmpty else { return [] }
        var records: [ImportSessionRecord] = []
        for ordinal in 0 ..< Self.historyDepth {
            let identifier = MockIdentifiers.importID(seed: configuration.seed, ordinal: ordinal)
            guard !isImportDismissed(identifier) else { continue }
            let scope = scopes[ordinal % scopes.count]
            let resolved = try await store.resolveDefaultAlbum(for: scope)
            records.append(record(identifier: identifier, ordinal: ordinal, scope: scope, album: resolved))
        }
        return Array(records.prefix(limit))
    }

    /// Rebuild a confirmable plan from a past run.
    ///
    /// Re-scans rather than replaying the stored decisions, because the library
    /// has moved on: what was an import last week may be a duplicate today, and
    /// a plan that said otherwise would be confidently wrong on the one screen
    /// whose whole job is to be right before anything is written.
    public func replan(_ importID: ImportID) async throws -> ImportPlan {
        let records = try await history(limit: Self.historyDepth)
        let scopes = try await availableScopes()
        let scope = records.first { $0.id == importID }?.scope ?? scopes.first
        guard let scope else {
            throw CapsuleError(
                code: .syncCursorInvalid,
                detail: "CapsuleMock: no import session \(importID.rawValue) to re-run"
            )
        }
        let scan = try await scan(scope)
        return try await plan(scan, destination: nil, mode: .copy, uploadPolicy: .full, streaming: false)
    }

    /// Forget the record. The assets it brought in are untouched.
    public func dismissSession(_ importID: ImportID) async throws {
        markImportDismissed(importID)
    }

    // MARK: Derivation

    private func record(
        identifier: ImportID,
        ordinal: Int,
        scope: ImportScope,
        album: (album: ContainerAlbum, rule: ImportPlan.DestinationRule)
    ) -> ImportSessionRecord {
        let hash = MockHash.value(seed: configuration.seed, index: ordinal, salt: .trip)
        let count = MockHash.integer(hash, in: 8 ... 240)
        let wasCancelled = MockHash.occurs(MockHash.mix(hash), perMille: 140)
        let startedAt = configuration.clock.offset(days: -(ordinal * 3 + 1))
        return ImportSessionRecord(
            id: identifier,
            scope: scope,
            destinationAlbumID: album.album.id,
            destinationRule: album.rule,
            mode: MockHash.occurs(MockHash.mix(hash &+ 1), perMille: 200) ? .move : .copy,
            startedAt: startedAt,
            finishedAt: CapsuleTimestamp(epochSeconds: startedAt.epochSeconds + 240),
            summary: ImportSummary(
                id: identifier,
                results: (0 ..< count).map { position in
                    Self.historicResult(seed: configuration.seed, ordinal: ordinal, position: position, scope: scope)
                }
            ),
            wasCancelled: wasCancelled
        )
    }

    /// One item's fate in a past run.
    ///
    /// A minority failed, which is what makes the re-run affordance mean
    /// something: a history where everything always worked would never show the
    /// retry path that is the reason the outcome is recorded per item at all.
    private static func historicResult(
        seed: UInt64,
        ordinal: Int,
        position: Int,
        scope: ImportScope
    ) -> ImportResult {
        let hash = MockHash.value(seed: seed, index: ordinal &* 1009 &+ position, salt: .representation)
        let locator = "\(scope.locator)/IMG_\(MockHash.hex(UInt64(position), digits: 4)).HEIC"
        if MockHash.occurs(hash, perMille: 55) {
            return ImportResult(locator: locator, outcome: .failed(.uploadChecksumMismatch))
        }
        if MockHash.occurs(MockHash.mix(hash), perMille: 90) {
            return ImportResult(locator: locator, outcome: .duplicateSkipped(existingAssetID: MockHash.hex(hash, digits: 12)))
        }
        return ImportResult(
            locator: locator,
            outcome: .imported(assetID: MockHash.hex(hash, digits: 12), derivativesDeferred: MockHash.occurs(hash >> 7, perMille: 40))
        )
    }
}
