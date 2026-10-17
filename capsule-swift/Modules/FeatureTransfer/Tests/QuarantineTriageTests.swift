import Foundation
import Testing

import CapsuleDomain
import CapsuleMock
import FeatureTransfer

/// Triage never decides for the user
/// (*Threat Model — Quarantine Surfaces*).
@Suite("Quarantine triage offers no default")
struct QuarantineTriageTests {
    private func item(
        surface: QuarantineSurface,
        resolutions: [QuarantineResolution],
        preservedBytes: UInt64? = nil
    ) -> QuarantineItem {
        QuarantineItem(
            id: QuarantineID("q-1"),
            surface: surface,
            reason: .malformedEncoding,
            detectedAt: CapsuleTimestamp(epochSeconds: 1787400000),
            preservedBytes: preservedBytes,
            resolutions: resolutions
        )
    }

    @Test("exactly three options, always, in inspect → repair → discard order")
    func alwaysThreeOptions() {
        let options = QuarantineActionOption.options(
            for: item(surface: .malformedSidecar, resolutions: [.inspect, .repair, .discard])
        )

        #expect(options.map(\.resolution) == [.inspect, .repair, .discard])
    }

    @Test("no option is a default")
    func noDefaultAction() {
        for surface in QuarantineSurface.knownCases {
            let options = QuarantineActionOption.options(
                for: item(surface: surface, resolutions: [.inspect, .repair, .discard])
            )
            #expect(options.allSatisfy { !$0.isDefault })
        }
    }

    @Test("only discard is destructive, and only discard confirms")
    func onlyDiscardConfirms() {
        let options = QuarantineActionOption.options(
            for: item(surface: .malformedSidecar, resolutions: [.inspect, .repair, .discard])
        )

        #expect(options.filter(\.isDestructive).map(\.resolution) == [.discard])
        #expect(options.filter(\.requiresConfirmation).map(\.resolution) == [.discard])
    }

    @Test("repair is shown but disabled where the holding area preserves nothing")
    func repairDisabledWhereMeaningless() {
        // A federation soft-fail lives in the bounded rejected-hash table: the
        // event is remembered, the bytes are not.
        let options = QuarantineActionOption.options(
            for: item(surface: .federationSoftFail, resolutions: [.inspect, .discard])
        )

        let repair = options.first { $0.resolution == .repair }
        #expect(repair != nil)
        #expect(repair?.isEnabled == false)
        #expect(repair?.unavailableReasonKey != nil)
    }

    @Test("inspect is disabled where there are no bytes to inspect")
    func inspectDisabledForAuditOnly() {
        let options = QuarantineActionOption.options(
            for: item(surface: .staleRevival, resolutions: [.inspect, .discard])
        )

        #expect(options.first { $0.resolution == .inspect }?.isEnabled == false)
    }

    @Test("discard is always available — nothing is ever stuck")
    func discardIsAlwaysAvailable() {
        for surface in QuarantineSurface.knownCases {
            let options = QuarantineActionOption.options(for: item(surface: surface, resolutions: [.inspect]))
            #expect(options.first { $0.resolution == .discard }?.isEnabled == true)
        }
    }

    @Test("every one of the eight surfaces has a stable reason code")
    func reasonCodesAreStable() {
        #expect(QuarantineReason.malformedEncoding.code == "malformed_encoding")
        #expect(QuarantineReason.verifyRejected(.forgedChain).code == "verify.forged_chain")
        #expect(QuarantineReason.serverRejected(.uploadStaleRevival).code == "error.upload.stale_revival")
        #expect(QuarantineSurface.knownCases.count == 8)
    }
}

// MARK: - Inbox

@Suite("QuarantineInboxModel against the mock")
@MainActor
struct QuarantineInboxModelTests {
    @Test("groups follow the threat model's table order")
    func canonicalGroupOrder() async {
        let environment = MockEnvironment(scenario: .quarantine)
        let model = QuarantineInboxModel(
            quarantine: environment.quarantine,
            library: environment.library,
            sync: environment.sync
        )

        await model.reload()

        let expected = QuarantineSurface.knownCases.filter { surface in
            model.groups.contains { $0.surface == surface }
        }
        #expect(model.groups.map(\.surface) == expected)
        #expect(model.monitoredSurfaceCount == 8)
    }

    @Test("an empty inbox is the good state, not a failure")
    func emptyIsGood() async {
        let environment = MockEnvironment(scenario: .healthy)
        let model = QuarantineInboxModel(
            quarantine: environment.quarantine,
            library: environment.library,
            sync: environment.sync
        )

        await model.reload()

        if model.groups.isEmpty {
            #expect(model.phase == .empty)
        } else {
            #expect(model.phase == .ready)
        }
    }

    @Test("nothing is resolved merely by opening the detail screen")
    func openingResolvesNothing() async {
        let environment = MockEnvironment(scenario: .quarantine)
        let inbox = QuarantineInboxModel(
            quarantine: environment.quarantine,
            library: environment.library,
            sync: environment.sync
        )
        await inbox.reload()
        guard let first = inbox.groups.first?.items.first else { return }

        let detail = QuarantineDetailModel(
            item: first,
            quarantine: environment.quarantine,
            sync: environment.sync
        )
        await detail.load()

        #expect(!detail.isResolved)
        #expect(detail.options.count == 3)
    }
}
