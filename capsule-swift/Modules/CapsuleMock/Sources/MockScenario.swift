import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockScenario

/// The whole world's state, as one closed choice.
///
/// Around thirty of the app's screens — grace-expired quota, a populated
/// quarantine, a federated album with an unreachable origin, the
/// "created with a newer version" indicator — are not reachable from a healthy
/// library at all. A scenario is how they are reached, and the UI tests select
/// one with a launch argument.
///
/// **The raw values are the contract.** A UI-test bundle links the app as a
/// target rather than as a library, so it cannot import this module and keeps
/// its own mirror of these strings. A mismatch has to fail loudly at launch
/// rather than quietly testing the wrong world, which is why parsing an unknown
/// name falls back to ``healthy`` only through
/// ``resolve(fromArguments:)`` and never silently inside `init`.
public enum MockScenario: String, Sendable, Equatable, Hashable, CaseIterable {
    /// A well-populated, fully synced library. The default.
    case healthy
    /// Signed in, nothing imported yet — every empty state.
    case emptyLibrary = "empty-library"
    /// No session on this device.
    case neverSignedIn = "never-signed-in"
    /// No usable network. Every local read still answers.
    case offline
    /// 250 000 assets, for the virtualized timeline.
    case hugeLibrary = "huge-library"
    /// Past the soft limit: uploads still work, the UI warns.
    case quotaSoftWarning = "quota-soft-warning"
    /// Past the hard limit for longer than the grace window.
    case quotaGraceExpired = "quota-grace-expired"
    /// Several distinct quarantine surfaces populated at once.
    case quarantine
    /// An aggregated album whose origins are unreachable, and peers with open
    /// circuits.
    case degradedFederation = "degraded-federation"
    /// Staged uploads in flight; originals still on the device that took them.
    case awaitingOriginals = "awaiting-originals"
    /// Documents written by a newer client — unknown closed-enum values and a
    /// schema this build will not write.
    case newerVersionState = "newer-version-state"
    /// Assets this build cannot open, for reasons that are not the asset's
    /// fault.
    case undecodableAssets = "undecodable-assets"
    /// The recovery-verification cadence has lapsed and snoozes are exhausted.
    case recoveryOverdue = "recovery-overdue"
    /// The server speaks a protocol version this build does not.
    case protocolUpgradeRequired = "protocol-upgrade-required"

    /// The launch argument the composition root and the UI tests agree on.
    public static let launchArgument = "-mock-scenario"

    /// Read the scenario from a process's arguments.
    ///
    /// Falls back to ``healthy`` for an absent or unrecognised name: a
    /// developer running the app with no arguments must get a working library,
    /// and a typo in a test's argument shows up as the healthy world rather than
    /// a crash on launch.
    public static func resolve(fromArguments arguments: [String]) -> MockScenario {
        guard let position = arguments.firstIndex(of: launchArgument),
              position + 1 < arguments.count,
              let scenario = MockScenario(rawValue: arguments[position + 1])
        else { return .healthy }
        return scenario
    }

    /// Read the scenario from the running process.
    public static func resolve(from processInfo: ProcessInfo = .processInfo) -> MockScenario {
        resolve(fromArguments: processInfo.arguments)
    }
}

// MARK: - MockClock

/// The injected clock.
///
/// Nothing in this module calls `Date()`. Every retention countdown, session
/// expiry, staleness check, and quota grace window is measured against this, so
/// a scenario renders identically today and in a year — and a test that asserts
/// "three days remaining" keeps passing.
public struct MockClock: Sendable, Equatable, Hashable {
    public var now: CapsuleTimestamp

    public init(now: CapsuleTimestamp) {
        self.now = now
    }

    /// 2026-08-22T12:00:00Z — the instant every scenario is anchored on.
    public static let referenceEpochSeconds: Int64 = 1_787_400_000

    /// The default clock.
    public static let reference = MockClock(
        now: CapsuleTimestamp(epochSeconds: referenceEpochSeconds)
    )

    /// The UTC day the newest photographs were taken on.
    public var todayDayNumber: Int64 {
        MockCalendar.dayNumber(epochSeconds: now.epochSeconds)
    }

    /// An instant a whole number of days from now, for expiries and deadlines.
    public func offset(days: Int) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: now.epochSeconds + Int64(days) * 86400)
    }

    /// An instant a whole number of seconds from now.
    public func offset(seconds: Int64) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: now.epochSeconds + seconds)
    }
}

// MARK: - MockBehaviour

/// Simulated latency and failure.
///
/// **Off by default, and deterministic when on.** A mock that randomly failed
/// would make every test flaky and every demo unreliable; one that cannot fail
/// at all leaves the error states unreachable. So failures are counted, not
/// sampled: the *n*-th call to a gated port fails, every time, with a named
/// code.
public struct MockBehaviour: Sendable, Equatable, Hashable {
    /// Artificial delay applied to gated calls.
    public var latencyNanoseconds: UInt64
    /// Fail every *n*-th gated call. `nil` never fails.
    public var failEveryNthCall: Int?
    /// The code a simulated failure carries.
    public var failureCode: ErrorCode

    public init(
        latencyNanoseconds: UInt64 = 0,
        failEveryNthCall: Int? = nil,
        failureCode: ErrorCode = .syncCursorInvalid
    ) {
        self.latencyNanoseconds = latencyNanoseconds
        self.failEveryNthCall = failEveryNthCall
        self.failureCode = failureCode
    }

    /// No latency, no failures. What every scenario uses unless it says
    /// otherwise.
    public static let deterministic = MockBehaviour()

    /// Every gated call fails with the given code — how ``MockScenario/offline``
    /// and ``MockScenario/protocolUpgradeRequired`` make their refusals real
    /// rather than cosmetic.
    public static func alwaysFailing(_ code: ErrorCode) -> MockBehaviour {
        MockBehaviour(failEveryNthCall: 1, failureCode: code)
    }
}

// MARK: - MockGate

/// Applies ``MockBehaviour`` to the calls that would touch a network.
///
/// Deliberately **not** applied to local reads. The offline-first contract is
/// that a gallery read never attempts the network, so gating one would model a
/// system Capsule is not — and would make the offline scenario fail in exactly
/// the place it is supposed to keep working.
public actor MockGate {
    private var behaviour: MockBehaviour
    private var callCount = 0

    public init(behaviour: MockBehaviour = .deterministic) {
        self.behaviour = behaviour
    }

    public func setBehaviour(_ behaviour: MockBehaviour) {
        self.behaviour = behaviour
        callCount = 0
    }

    /// Admit one gated call.
    ///
    /// - Throws: ``CapsuleError`` carrying the configured code when this call is
    ///   the *n*-th.
    public func admit() async throws {
        callCount += 1
        if behaviour.latencyNanoseconds > 0 {
            try? await Task.sleep(nanoseconds: behaviour.latencyNanoseconds)
        }
        guard let interval = behaviour.failEveryNthCall, interval > 0 else { return }
        guard callCount.isMultiple(of: interval) else { return }
        throw CapsuleError(
            code: behaviour.failureCode,
            detail: "CapsuleMock: injected failure on gated call \(callCount)"
        )
    }
}
