import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - ImportPort

/// Scan → plan → execute, as three calls.
///
/// The middle one is a decision point, not a formality. A plan names the
/// destination, the mode, and every per-candidate outcome *before* anything is
/// written, which is the only way a user can meaningfully consent to
/// ``ImportMode/move`` — an operation that deletes their source files.
extension MockTransferStore: ImportPort {
    public func availableScopes() async throws -> [ImportScope] {
        let platform = PlatformTag(rawValue: PlatformEnvironment.platformTag)
        return [
            scope(platform: platform, kind: .cameraRoll, locator: "photokit://camera-roll", ordinal: 0),
            scope(platform: platform, kind: .screenshots, locator: "photokit://screenshots", ordinal: 1),
            scope(platform: platform, kind: .folder, locator: "file:///Volumes/Photos/2026", ordinal: 2),
            scope(platform: platform, kind: .removableVolume, locator: "file:///Volumes/SD-CARD", ordinal: 3),
        ]
    }

    /// Enumerate a source. Reads nothing into the library.
    ///
    /// Unreadable locators are **surfaced, not skipped**: a permissions problem
    /// is a different thing from an unsupported format, and a user who imports
    /// four hundred files and sees three hundred and eighty arrive is owed the
    /// reason for the other twenty.
    public func scan(_ scope: ImportScope) async throws -> ImportScan {
        let count = MockHash.integer(
            MockHash.value(seed: configuration.seed, index: scope.scopeID.utf8.count, salt: .byteSize),
            in: 12 ... 90
        )
        let candidates = (0 ..< count).map { candidate(scope: scope, ordinal: $0) }
        return ImportScan(
            scope: scope,
            candidates: candidates,
            unreadableLocators: count > 40 ? ["\(scope.locator)/locked/IMG_0042.HEIC"] : []
        )
    }

    /// Turn a scan into a plan.
    ///
    /// Rejects a ``UploadPolicy/staged`` policy combined with streaming
    /// **outright** rather than silently choosing one: streaming exists to
    /// release local bytes quickly, staged defers exactly the upload that
    /// release depends on, and a client that picked a winner would be deleting
    /// source files against a promise it had quietly broken.
    public func plan(
        _ scan: ImportScan,
        destination: AlbumID?,
        mode: ImportMode,
        uploadPolicy: UploadPolicy,
        streaming: Bool
    ) async throws -> ImportPlan {
        try mode.requireWritable()
        guard !(streaming && uploadPolicy == .staged) else {
            throw CapsuleError(
                code: .uploadInvalidAction,
                detail: "CapsuleMock: streaming import is mutually exclusive with a staged upload policy"
            )
        }
        let resolved = try await resolveDestination(destination, scope: scan.scope)
        return ImportPlan(
            id: MockIdentifiers.importID(seed: configuration.seed, ordinal: scan.candidates.count),
            scope: scan.scope,
            destinationAlbumID: resolved.album.id,
            destinationRule: resolved.rule,
            mode: mode,
            uploadPolicy: uploadPolicy,
            isStreaming: streaming,
            decisions: scan.candidates.map { decision(for: $0) }
        )
    }

    /// Execute a confirmed plan, streaming progress.
    ///
    /// The stream is the whole interface to a running import. Cancelling is a
    /// **stop, not a rollback**: everything already imported stays imported, and
    /// the summary says so — which is why the cancelled event carries the same
    /// tally the finished one does.
    public nonisolated func execute(_ plan: ImportPlan) -> AsyncStream<ImportProgressEvent> {
        AsyncStream { continuation in
            Task {
                continuation.yield(.started(importID: plan.id, totalCandidates: plan.decisions.count))
                var results: [ImportResult] = []
                for (position, decision) in plan.decisions.enumerated() {
                    if await self.isImportCancelled(plan.id) {
                        continuation.yield(.cancelled(summary: ImportSummary(id: plan.id, results: results)))
                        continuation.finish()
                        return
                    }
                    continuation.yield(.candidateStarted(
                        index: position,
                        total: plan.decisions.count,
                        locator: decision.candidate.locator
                    ))
                    let outcome = Self.outcome(for: decision)
                    results.append(ImportResult(locator: decision.candidate.locator, outcome: outcome))
                    continuation.yield(.candidateFinished(
                        index: position,
                        locator: decision.candidate.locator,
                        outcome: outcome
                    ))
                }
                continuation.yield(.finished(summary: ImportSummary(id: plan.id, results: results)))
                continuation.finish()
            }
        }
    }

    public func cancel(_ importID: ImportID) async throws {
        markImportCancelled(importID)
    }

    // MARK: Derivation

    private func resolveDestination(
        _ destination: AlbumID?,
        scope: ImportScope
    ) async throws -> (album: ContainerAlbum, rule: ImportPlan.DestinationRule) {
        if let destination, let album = await store.container(destination) {
            return (album, .explicitUserPick)
        }
        return try await store.resolveDefaultAlbum(for: scope)
    }

    private func scope(
        platform: PlatformTag,
        kind: SourceKind,
        locator: String,
        ordinal: Int
    ) -> ImportScope {
        // The scope id is computed deterministically in `capsule-core` from
        // `(platform, source_kind, locator)`; it is never recomputed in Swift,
        // so the mock derives a stand-in rather than reimplementing a value two
        // devices must agree on byte-for-byte.
        ImportScope(
            scopeID: MockHash.hex(
                MockHash.value(seed: configuration.seed, index: ordinal, salt: .identity),
                digits: 16
            ),
            platform: platform,
            sourceKind: kind,
            locator: locator
        )
    }

    private func candidate(scope: ImportScope, ordinal: Int) -> ImportCandidate {
        let hash = MockHash.value(seed: configuration.seed, index: ordinal, salt: .contentType, sub: 77)
        let type = MockHash.element(hash, from: ContentType.knownCases) ?? .heic
        let hasCompanion = MockHash.occurs(MockHash.mix(hash), perMille: 180)
        return ImportCandidate(
            id: "\(scope.scopeID)-\(ordinal)",
            locator: "\(scope.locator)/IMG_\(MockHash.hex(UInt64(4000 + ordinal), digits: 4)).\(Self.suffix(type))",
            contentType: type,
            byteSize: UInt64(MockHash.integer(MockHash.mix(hash &+ 3), in: 900_000 ... 48_000_000)),
            companionLocators: hasCompanion ? ["\(scope.locator)/IMG_\(ordinal).JPG"] : []
        )
    }

    /// Every candidate gets an explicit decision — there is no silently-skipped
    /// bucket.
    private func decision(for candidate: ImportCandidate) -> ImportDecision {
        let hash = MockHash.value(seed: configuration.seed, index: candidate.id.utf8.count, salt: .stacking, sub: 31)
        if !candidate.companionLocators.isEmpty {
            return ImportDecision(candidate: candidate, action: .importAsStackMember(stackType: .rawJpeg, role: .primary))
        }
        if MockHash.occurs(hash, perMille: 70) {
            return ImportDecision(candidate: candidate, action: .skipDuplicate(existingAssetID: candidate.id))
        }
        if MockHash.occurs(MockHash.mix(hash), perMille: 40) {
            return ImportDecision(candidate: candidate, action: .skipUnsupported(candidate.contentType))
        }
        return ImportDecision(candidate: candidate, action: .importAsset)
    }

    /// A deferred derivative is a **successful** import: the original is signed,
    /// encrypted, and verifiable, and only the thumbnail is missing because this
    /// build has no codec. Counting it as a failure would make a HEIC-only
    /// library look like it lost data.
    private static func outcome(for decision: ImportDecision) -> ImportOutcome {
        switch decision.action {
        case .importAsset:
            .imported(assetID: decision.candidate.id, derivativesDeferred: false)
        case let .importAsStackMember(_, role):
            .imported(assetID: decision.candidate.id, derivativesDeferred: role == .proxy)
        case let .skipDuplicate(existing):
            .duplicateSkipped(existingAssetID: existing)
        case .skipUnsupported:
            .unsupported
        case .skipUnreadable:
            .unreadable
        }
    }

    private static func suffix(_ type: ContentType) -> String {
        switch type {
        case .heic: "HEIC"
        case .jpeg: "JPG"
        case .dng: "DNG"
        case .png: "PNG"
        case .mp4: "MP4"
        case .quicktime: "MOV"
        default: "DAT"
        }
    }
}
