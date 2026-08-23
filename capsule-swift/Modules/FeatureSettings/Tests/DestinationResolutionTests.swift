import CapsuleDomain
import CapsuleFoundation
import FeatureSettings
import Foundation
import Testing

// MARK: - DestinationFixture

/// The five album identifiers the precedence ladder is exercised with, at file
/// scope so a table-driven case can name them.
enum DestinationFixture {
    static let pick = AlbumID.managed(uuid: "album-pick")
    static let scopeOverride = AlbumID.managed(uuid: "album-override")
    static let kindDefault = AlbumID.managed(uuid: "album-kind")
    static let pointer = AlbumID.managed(uuid: "album-pointer")
    static let derived = AlbumID.managed(uuid: "album-derived")
}

/// One row of the precedence table: which rungs are configured, and what the
/// ladder must answer.
struct ResolutionSample: Sendable {
    var hasPick = false
    var hasOverride = false
    var hasKind = false
    var hasPointer = false
    var rule: ImportPlan.DestinationRule = .derivedDefaultAlbum
    var album: AlbumID = DestinationFixture.derived
}

// MARK: - DestinationResolutionTests

/// A user asking "why did that photo land *there*" is asking which rule fired,
/// so resolution reports the rung as well as the album.
@Suite("Import destinations resolve by a fixed precedence, first match wins")
struct DestinationResolutionTests {
    @Test("the ladder is the five documented rungs, highest precedence first")
    func ladderIsTheDocumentedOne() {
        #expect(DestinationResolution.order == [
            .explicitUserPick,
            .scopeOverride,
            .sourceKindDefault,
            .ownerDefaultPointer,
            .derivedDefaultAlbum,
        ])
        #expect(Set(DestinationResolution.order).count == 5, "a rung listed twice is a precedence written twice")
    }

    @Test(
        "each rung fires when it is the highest one configured",
        arguments: [
            ResolutionSample(hasPick: true, hasOverride: true, hasKind: true, hasPointer: true, rule: .explicitUserPick),
            ResolutionSample(hasOverride: true, hasKind: true, hasPointer: true, rule: .scopeOverride),
            ResolutionSample(hasKind: true, hasPointer: true, rule: .sourceKindDefault),
            ResolutionSample(hasPointer: true, rule: .ownerDefaultPointer),
            ResolutionSample(rule: .derivedDefaultAlbum),
        ]
    )
    func highestConfiguredRungWins(sample: ResolutionSample) {
        let rule = DestinationResolution.rule(
            explicitPick: sample.hasPick ? DestinationFixture.pick : nil,
            scopeOverride: sample.hasOverride ? DestinationFixture.scopeOverride : nil,
            sourceKindDefault: sample.hasKind ? DestinationFixture.kindDefault : nil,
            ownerPointer: sample.hasPointer ? DestinationFixture.pointer : nil
        )

        #expect(rule == sample.rule)
    }

    /// The tie-breaks: each rung must beat every rung below it in isolation,
    /// not merely win when it is the only one set.
    @Test("a higher rung beats a lower one even when only those two are configured")
    func higherRungBeatsEachLowerOne() {
        #expect(DestinationResolution.rule(
            explicitPick: DestinationFixture.pick,
            scopeOverride: nil,
            sourceKindDefault: nil,
            ownerPointer: DestinationFixture.pointer
        ) == .explicitUserPick)

        #expect(DestinationResolution.rule(
            explicitPick: nil,
            scopeOverride: DestinationFixture.scopeOverride,
            sourceKindDefault: DestinationFixture.kindDefault,
            ownerPointer: nil
        ) == .scopeOverride)

        #expect(DestinationResolution.rule(
            explicitPick: nil,
            scopeOverride: nil,
            sourceKindDefault: DestinationFixture.kindDefault,
            ownerPointer: DestinationFixture.pointer
        ) == .sourceKindDefault)

        #expect(DestinationResolution.rule(
            explicitPick: DestinationFixture.pick,
            scopeOverride: DestinationFixture.scopeOverride,
            sourceKindDefault: nil,
            ownerPointer: nil
        ) == .explicitUserPick)
    }

    @Test("resolution names the album and the rule that chose it")
    func resolutionReportsWhichRuleFired() {
        let resolved = DestinationResolution.destination(
            explicitPick: nil,
            scopeOverride: DestinationFixture.scopeOverride,
            sourceKindDefault: DestinationFixture.kindDefault,
            ownerPointer: DestinationFixture.pointer,
            derivedDefault: DestinationFixture.derived
        )

        #expect(resolved.rule == .scopeOverride)
        #expect(resolved.album == DestinationFixture.scopeOverride, "the album must come from the rung that fired")
    }

    @Test(
        "the album returned is always the one belonging to the winning rung",
        arguments: [
            ResolutionSample(hasPick: true, album: DestinationFixture.pick),
            ResolutionSample(hasOverride: true, album: DestinationFixture.scopeOverride),
            ResolutionSample(hasKind: true, album: DestinationFixture.kindDefault),
            ResolutionSample(hasPointer: true, album: DestinationFixture.pointer),
            ResolutionSample(album: DestinationFixture.derived),
        ]
    )
    func winningRungSuppliesTheAlbum(sample: ResolutionSample) {
        let resolved = DestinationResolution.destination(
            explicitPick: sample.hasPick ? DestinationFixture.pick : nil,
            scopeOverride: sample.hasOverride ? DestinationFixture.scopeOverride : nil,
            sourceKindDefault: sample.hasKind ? DestinationFixture.kindDefault : nil,
            ownerPointer: sample.hasPointer ? DestinationFixture.pointer : nil,
            derivedDefault: DestinationFixture.derived
        )

        #expect(resolved.album == sample.album)
    }

    /// The floor has no input, so resolution cannot fall off the end.
    @Test("with nothing configured the derived default album is the floor")
    func derivedDefaultIsTheFloor() {
        let resolved = DestinationResolution.destination(
            explicitPick: nil,
            scopeOverride: nil,
            sourceKindDefault: nil,
            ownerPointer: nil,
            derivedDefault: DestinationFixture.derived
        )

        #expect(resolved.rule == .derivedDefaultAlbum)
        #expect(resolved.album == DestinationFixture.derived)
    }

    @Test("a library with no derived album yet still reports the rung rather than hiding the row")
    func missingDerivedAlbumStillReportsARung() {
        let resolved = DestinationResolution.destination(
            explicitPick: nil,
            scopeOverride: nil,
            sourceKindDefault: nil,
            ownerPointer: nil,
            derivedDefault: nil
        )

        #expect(resolved.rule == .derivedDefaultAlbum)
        #expect(resolved.album == nil)
    }

    @Test("every rung is named by its own catalog key")
    func everyRungHasACatalogKey() {
        let keys = DestinationResolution.order.map(DestinationResolution.titleKey(for:))

        #expect(Set(keys).count == keys.count)
        for key in keys {
            #expect(key.hasPrefix("app.settings.import.rule."))
            #expect(!key.contains(" "))
        }
    }

    @Test("every source kind is named, the unknown one included", arguments: SourceKind.knownCases)
    func everySourceKindHasACatalogKey(kind: SourceKind) {
        let key = DestinationResolution.titleKey(for: kind)

        #expect(key.hasPrefix("app.settings.import.kind."))
        #expect(!key.contains(" "))
    }

    @Test("a source kind from a newer writer still gets a key rather than a crash")
    func unknownSourceKindIsMapped() {
        let known = SourceKind.knownCases.map(DestinationResolution.titleKey(for:))
        let unknown = DestinationResolution.titleKey(for: SourceKind(rawValue: "holo_camera"))

        #expect(unknown == "app.settings.import.kind.unknown")
        #expect(Set(known).count == SourceKind.knownCases.count)
    }
}
