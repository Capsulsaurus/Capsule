import CapsuleDomain
import CapsuleMock
import Foundation

// MARK: - PreviewCeremonyBehaviour

/// Which of the ceremony's documented outcomes a preview wants.
///
/// Every one of these is a *specified* failure mode of
/// *Device Enrollment — Failure Modes*, not an invented one — which is the test
/// of whether a mock is modelling the system or a fantasy of it.
public struct PreviewCeremonyBehaviour: Sendable, Equatable, Hashable {
    /// The secure element refuses to generate keys.
    public var hardwareKeysFail: Bool
    /// The directory upload cannot reach the server. The device stays locally
    /// functional and finishes when the server is back.
    public var directoryUploadDefers: Bool
    /// Default-album creation fails. Any device recreates it lazily before the
    /// first import, so setup still completes.
    public var defaultAlbumDefers: Bool
    /// Whether this device has a secure element at all.
    public var availability: HardwareKeyAvailability

    public init(
        hardwareKeysFail: Bool = false,
        directoryUploadDefers: Bool = false,
        defaultAlbumDefers: Bool = false,
        availability: HardwareKeyAvailability = .secureElement
    ) {
        self.hardwareKeysFail = hardwareKeysFail
        self.directoryUploadDefers = directoryUploadDefers
        self.defaultAlbumDefers = defaultAlbumDefers
        self.availability = availability
    }

    public static let healthy = PreviewCeremonyBehaviour()
    /// The hardware-refusal path, which must offer Retry or software keys.
    public static let hardwareRefusal = PreviewCeremonyBehaviour(hardwareKeysFail: true)
    /// The unreachable-server path, which must not block setup.
    public static let serverUnreachable = PreviewCeremonyBehaviour(
        directoryUploadDefers: true,
        defaultAlbumDefers: true
    )
}

// MARK: - PreviewEnrollmentCeremony

/// A ``FirstDeviceEnrollmentPort`` that walks the six stages.
public actor PreviewEnrollmentCeremony: FirstDeviceEnrollmentPort {
    private let behaviour: PreviewCeremonyBehaviour
    private var isCancelled = false

    public init(behaviour: PreviewCeremonyBehaviour = .healthy) {
        self.behaviour = behaviour
    }

    public func hardwareKeyAvailability() async -> HardwareKeyAvailability {
        behaviour.availability
    }

    public nonisolated func run(allowingSoftwareKeys: Bool) -> AsyncStream<EnrollmentStageEvent> {
        AsyncStream { continuation in
            Task {
                await self.resume()
                for stage in EnrollmentStage.allCases {
                    guard await !self.cancelled else {
                        continuation.finish()
                        return
                    }
                    continuation.yield(EnrollmentStageEvent(stage: stage, status: .running))
                    let status = await self.outcome(for: stage, allowingSoftwareKeys: allowingSoftwareKeys)
                    continuation.yield(EnrollmentStageEvent(stage: stage, status: status))
                    if case .failed = status {
                        continuation.finish()
                        return
                    }
                }
                continuation.finish()
            }
        }
    }

    public func cancel() async {
        isCancelled = true
    }

    private var cancelled: Bool { isCancelled }

    private func resume() {
        isCancelled = false
    }

    private func outcome(
        for stage: EnrollmentStage,
        allowingSoftwareKeys: Bool
    ) -> EnrollmentStageStatus {
        switch stage {
        case .deviceKeys where behaviour.hardwareKeysFail && !allowingSoftwareKeys:
            .failed(.hardwareKeyUnavailable)
        case .publishDirectory where behaviour.directoryUploadDefers:
            .deferred(reasonKey: "ios.enrollment.deferred.directory")
        case .defaultAlbum where behaviour.defaultAlbumDefers:
            .deferred(reasonKey: "ios.enrollment.deferred.default_album")
        default:
            .done
        }
    }
}
