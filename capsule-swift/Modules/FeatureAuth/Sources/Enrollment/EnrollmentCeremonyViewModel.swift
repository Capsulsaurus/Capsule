import CapsuleDomain
import Foundation
import Observation

// MARK: - EnrollmentStageRow

/// One row of the named step rail.
public struct EnrollmentStageRow: Sendable, Equatable, Hashable, Identifiable {
    public var stage: EnrollmentStage
    public var status: EnrollmentStageStatus

    public var id: String { stage.rawValue }

    public init(stage: EnrollmentStage, status: EnrollmentStageStatus) {
        self.stage = stage
        self.status = status
    }
}

// MARK: - EnrollmentCeremonyViewModel

/// Drives the six-step first-device ceremony.
///
/// **A named rail, not a percentage.** The steps of
/// *Device Enrollment — First-Device Enrollment* are things that can
/// individually succeed, individually fail, and individually be worth waiting
/// for. A percentage bar can express none of that: it cannot say "your device's
/// secure element refused, here is what that costs you", and it cannot say "the
/// directory upload will finish when the server is reachable — your keys are
/// already valid".
///
/// Two of the documented failure modes are explicitly **not** failures, and the
/// rail renders them as ``EnrollmentStageStatus/deferred(reasonKey:)``:
///
/// - a directory upload that cannot reach the server leaves the device locally
///   functional and invisible to other devices until it lands;
/// - a default album that cannot be created is recreated lazily by any device
///   before the first import, because its id is derivable from the master key.
///
/// Neither may block setup, so neither may present as a failure.
@MainActor
@Observable
public final class EnrollmentCeremonyViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var rows: [EnrollmentStageRow] = EnrollmentStage.allCases.map {
        EnrollmentStageRow(stage: $0, status: .pending)
    }

    public private(set) var hardwareAvailability: HardwareKeyAvailability = .softwareOnly
    public private(set) var isRunning = false
    /// Whether the user has been told what software keys cost and chose them
    /// anyway. A **documented deviation**, never a silent fallback.
    public private(set) var acceptedSoftwareKeyDeviation = false

    private let enrollment: any FirstDeviceEnrollmentPort
    private nonisolated(unsafe) var ceremony: Task<Void, Never>?

    public init(enrollment: any FirstDeviceEnrollmentPort) {
        self.enrollment = enrollment
    }

    deinit {
        ceremony?.cancel()
    }

    // MARK: Derived state

    /// The stage that stopped, if one did.
    public var failure: EnrollmentStageFailure? {
        for row in rows {
            if case let .failed(failure) = row.status { return failure }
        }
        return nil
    }

    /// The stage currently running, for the rail's live status line.
    public var activeStage: EnrollmentStage? {
        rows.first { $0.status == .running }?.stage
    }

    /// Stages that finished but left work outstanding.
    public var deferredStages: [EnrollmentStage] {
        rows.compactMap { row in
            if case .deferred = row.status { return row.stage }
            return nil
        }
    }

    /// Whether the ceremony got far enough that the account exists and works.
    ///
    /// Deliberately **not** "every stage is `.done`": a deferred directory
    /// upload or default album still means a fully valid account, and gating the
    /// hand-off on them would strand a user whose server is briefly down behind
    /// a screen they cannot leave.
    public var isComplete: Bool {
        rows.allSatisfy(\.status.isTerminal) && failure == nil
    }

    /// Whether the hardware-failure recovery should be offered.
    public var offersSoftwareKeyDeviation: Bool {
        failure?.offersSoftwareKeyDeviation ?? false
    }

    /// Whether this device will hold its classical key halves in a secure
    /// element. Named for what it is: the PQ half is software-sealed on every
    /// shipping secure element, so "hardware keys" would overclaim.
    public var usesSecureElement: Bool {
        hardwareAvailability == .secureElement && !acceptedSoftwareKeyDeviation
    }

    // MARK: Actions

    /// Ask what this device can do, before the rail claims anything.
    public func prepare() async {
        hardwareAvailability = await enrollment.hardwareKeyAvailability()
        state = .ready
    }

    /// Run the ceremony from the top.
    public func start() async {
        guard !isRunning else { return }
        resetRows()
        isRunning = true
        state = .loading
        let stream = enrollment.run(allowingSoftwareKeys: acceptedSoftwareKeyDeviation)
        for await event in stream {
            apply(event)
        }
        isRunning = false
        // `.ready` either way: a stage failure is rendered *in the rail*, beside
        // the step that failed and next to its recovery, not as a banner over a
        // ceremony the user can no longer see.
        state = .ready
    }

    /// Try the same ceremony again, unchanged. The first response to a hardware
    /// refusal, because secure elements do sometimes refuse transiently.
    public func retry() async {
        await start()
    }

    /// Accept software keys and run again.
    ///
    /// The deviation is recorded on the model so the summary screen — and the
    /// device row in Settings — can keep saying so afterwards. A user who
    /// accepted a weaker key custody once should not have to remember it.
    public func continueWithSoftwareKeys() async {
        acceptedSoftwareKeyDeviation = true
        await start()
    }

    /// Abandon. No state is persisted; the user starts over.
    public func cancel() async {
        ceremony?.cancel()
        await enrollment.cancel()
        isRunning = false
        resetRows()
        state = .idle
    }

    private func apply(_ event: EnrollmentStageEvent) {
        guard let index = rows.firstIndex(where: { $0.stage == event.stage }) else { return }
        rows[index].status = event.status
    }

    private func resetRows() {
        rows = EnrollmentStage.allCases.map { EnrollmentStageRow(stage: $0, status: .pending) }
    }
}
