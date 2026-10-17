import CapsuleDomain
import CapsuleFoundation
import FeatureSettings
import Foundation
import Testing

// MARK: - SettingsPhaseTests

/// Five states, one at a time. A screen that cannot tell "you have no enrolled
/// devices" from "we could not reach your server" is lying by omission.
@Suite("A settings screen is in exactly one of five phases")
struct SettingsPhaseTests {
    @Test("loading, ready, empty, offline, and failed are all distinguishable")
    func phasesDoNotOverlap() {
        #expect(SettingsPhase.loading.isLoading)
        #expect(!SettingsPhase.loading.isReady)
        #expect(SettingsPhase.ready.isReady)
        #expect(!SettingsPhase.ready.isLoading)
        #expect(!SettingsPhase.empty.isReady, "nothing to show is not the same as something to show")
        #expect(!SettingsPhase.offline.isReady)
        #expect(SettingsPhase.failed(.quotaExceeded) != SettingsPhase.failed(.syncCursorInvalid))
        #expect(SettingsPhase.offline != SettingsPhase.failed(.syncCursorInvalid))
    }

    @Test("the unclassified key is carried verbatim rather than coerced onto a real code")
    func unclassifiedKeyIsItsOwnThing() {
        #expect(SettingsPhase.unclassifiedErrorKey == "error.client.unclassified")
        #expect(ErrorCode.unknown(SettingsPhase.unclassifiedErrorKey).rawValue == "error.client.unclassified")
    }
}

// MARK: - SettingsConnectivityTests

/// Offline is not an error code — no server answers when the radio is off — so
/// the probe reads the connection class the sync port already tracks.
@Suite("Offline is told apart from failed by reading the connection, not the error")
struct SettingsConnectivityTests {
    private struct NotACapsuleError: Error {}

    @Test("a usable connection classifies a failure by its code")
    func usableConnectionReportsTheCode() async {
        let connectivity = SettingsConnectivity.stub(connection: .unmetered)

        let phase = await connectivity.phase(for: CapsuleError(code: .quotaExceeded))

        #expect(phase == .failed(.quotaExceeded))
        let isOffline = await connectivity.isOffline()
        #expect(!isOffline)
    }

    /// A request that failed while the radio was off failed *because* the radio
    /// was off, whatever code the transport invented on the way out.
    @Test("offline wins over whatever code the transport produced")
    func offlineWinsOverTheCode() async {
        let connectivity = SettingsConnectivity.stub(connection: .offline)

        let phase = await connectivity.phase(for: CapsuleError(code: .quotaExceeded))

        #expect(phase == .offline)
        let isOffline = await connectivity.isOffline()
        #expect(isOffline)
    }

    @Test("a metered or adverse connection is usable, so a failure is still a failure")
    func degradedConnectionsAreStillUsable() async {
        for connection in [ConnectionClass.metered, .constrained, .adverse] {
            let connectivity = SettingsConnectivity.stub(connection: connection)
            let phase = await connectivity.phase(for: CapsuleError(code: .syncCursorInvalid))
            #expect(phase == .failed(.syncCursorInvalid), "\(connection.rawValue) can still reach a server")
        }
    }

    /// Guessing "offline" from an unreadable state would blame the network for
    /// a bug.
    @Test("a connection class that cannot be read at all is not reported as offline")
    func unreadableConnectionIsNotOffline() async {
        let connectivity = SettingsConnectivity.stub(connection: nil)

        let connection = await connectivity.connectionClass()
        let isOffline = await connectivity.isOffline()
        let phase = await connectivity.phase(for: CapsuleError(code: .quotaExceeded))

        #expect(connection == nil)
        #expect(!isOffline)
        #expect(phase == .failed(.quotaExceeded))
    }

    @Test("something that is not a CapsuleError is carried as an unclassified key, not a plausible code")
    func foreignErrorsAreVisiblyUnclassified() async {
        let connectivity = SettingsConnectivity.stub(connection: .unmetered)

        let phase = await connectivity.phase(for: NotACapsuleError())

        #expect(phase == .failed(.unknown(SettingsPhase.unclassifiedErrorKey)))
    }

    @Test("the connection class is reported as it stands")
    func connectionClassIsReported() async {
        let connectivity = SettingsConnectivity.stub(connection: .metered)

        let connection = await connectivity.connectionClass()

        #expect(connection == .metered)
    }
}
