import CapsuleDomain
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - CrossDeviceAddView

/// Adding another device, from the device that is already enrolled.
///
/// The code appears in two forms because people are in two situations: a QR
/// carrying the full-entropy payload for a phone that can see this screen, and
/// an 8–10 digit fallback for one that cannot — read down a phone line, typed
/// across a room. The fallback trades entropy for transcribability and is safe
/// only because the code was never the security: it is single-use, expires in
/// ten minutes, and its redemption is rate-limited. Channel integrity comes from
/// the safety-code check that follows.
///
/// Both sides acknowledge explicitly, and this side's acknowledgement is held
/// against the ceremony rather than the clock: if the far device finishes before
/// the user has compared anything, completion is **withheld** until they do.
/// Reporting success over the top of a check still on screen would teach the
/// user that the check is decorative, which is the one lesson this screen must
/// never teach.
///
/// Entry point: ``init(enrollment:ceremony:now:)``, needing ``EnrollmentPort``
/// and ``CrossDeviceCeremonyPort``.
public struct CrossDeviceAddView: View {
    @State private var model: CrossDeviceAddViewModel

    public init(
        enrollment: any EnrollmentPort,
        ceremony: any CrossDeviceCeremonyPort,
        now: (@Sendable () -> CapsuleTimestamp)? = nil
    ) {
        if let now {
            _model = State(wrappedValue: CrossDeviceAddViewModel(
                enrollment: enrollment,
                ceremony: ceremony,
                now: now
            ))
        } else {
            _model = State(wrappedValue: CrossDeviceAddViewModel(
                enrollment: enrollment,
                ceremony: ceremony
            ))
        }
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: "ios.crossdevice.title",
                    subtitleKey: "ios.crossdevice.subtitle",
                    symbolName: "iphone.and.arrow.right.outward"
                )
                content
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.step {
        case .idle: idleStep
        case .awaitingRedemption: awaitingStep
        case .verifyingSafetyCode: safetyStep
        case .transferringKeys: AuthLoadingView(labelKey: "ios.crossdevice.transferring")
        case let .completed(identifier): completedStep(identifier)
        case .abortedOnMismatch: abortedStep
        case let .failed(error): failedStep(error)
        }
    }

    // MARK: Idle

    private var idleStep: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "ios.crossdevice.start.title",
                descriptionKey: "ios.crossdevice.start.description",
                symbolName: "qrcode"
            )
            if model.isWorking {
                AuthLoadingView(labelKey: "ios.crossdevice.issuing")
            }
            Button("ios.crossdevice.issue") { Task { await model.issueCode() } }
                .capsuleGlassButtonStyle(prominent: true)
                .disabled(model.isWorking)
                .accessibilityLabel("ios.crossdevice.issue")
        }
    }

    // MARK: Awaiting redemption

    @ViewBuilder
    private var awaitingStep: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
            AuthSectionHeader(
                titleKey: "ios.crossdevice.code.title",
                descriptionKey: "ios.crossdevice.code.description",
                symbolName: "qrcode"
            )
            qrCode
            fallbackCode
            liveness
            AuthLoadingView(labelKey: "ios.crossdevice.waiting")
            Button("ios.crossdevice.cancel") { Task { await model.cancel() } }
                .buttonStyle(.borderless)
                .accessibilityLabel("ios.crossdevice.cancel")
        }
    }

    @ViewBuilder
    private var qrCode: some View {
        if let payload = model.qrPayload() {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
                SecretQRCodeView(payload: payload)
                Text("ios.crossdevice.code.qr_note")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .authCard()
        }
    }

    /// The transcribable half. Grouped in threes, which is how a person reads a
    /// number aloud without losing their place.
    private var fallbackCode: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            AuthCodeValue(
                labelKey: "ios.crossdevice.code.fallback",
                code: model.textFallbackDisplay,
                font: .title2.monospaced().weight(.semibold)
            )
            Text("ios.crossdevice.code.fallback_note")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .authCard()
    }

    @ViewBuilder
    private var liveness: some View {
        if model.isInviteLive {
            StatusChip(
                titleKey: "ios.crossdevice.code.live",
                symbolName: "clock.fill",
                tint: .secondary
            )
        } else {
            StatusChip(
                titleKey: "ios.crossdevice.code.expired",
                symbolName: "clock.badge.xmark.fill",
                tint: .orange
            )
            Button("ios.crossdevice.issue_again") { Task { await model.issueCode() } }
                .buttonStyle(.bordered)
                .accessibilityLabel("ios.crossdevice.issue_again")
        }
    }

    // MARK: Safety check

    @ViewBuilder
    private var safetyStep: some View {
        if let check = model.safetyCheck {
            SafetyCodeCheckView(
                safetyCheck: check,
                safetyCodeDisplay: model.safetyCodeDisplay,
                hasAcknowledged: $model.hasAcknowledgedMatch,
                canConfirm: model.canConfirm,
                confirm: { Task { await model.confirmMatch() } },
                abort: { Task { await model.abortOnMismatch() } }
            )
        } else {
            AuthLoadingView(labelKey: "ios.crossdevice.safety.loading")
        }
    }

    // MARK: Terminal states

    private func completedStep(_ identifier: DeviceID) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "ios.crossdevice.completed.title",
                descriptionKey: "ios.crossdevice.completed.description",
                symbolName: "checkmark.seal.fill"
            )
            AuthCodeValue(
                labelKey: "ios.crossdevice.completed.device_id",
                code: ChunkedCodeFormatter.chunked(identifier.rawValue),
                font: .callout.monospaced()
            )
        }
        .authCard()
    }

    /// Loud, and terminal. A divergent code means something sat in the middle of
    /// the channel, so the code and the channel are both dead and the user
    /// starts over rather than retrying into the same relay.
    private var abortedStep: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "ios.crossdevice.aborted.title",
                descriptionKey: "ios.crossdevice.aborted.description",
                symbolName: "exclamationmark.octagon.fill"
            )
            Button("ios.crossdevice.aborted.restart") { Task { await model.issueCode() } }
                .capsuleGlassButtonStyle(prominent: true)
                .accessibilityLabel("ios.crossdevice.aborted.restart")
        }
        .authCard()
    }

    @ViewBuilder
    private func failedStep(_ error: AuthPresentableError) -> some View {
        // Issuing a code needs *fresh* local authorization, not merely a valid
        // session token, so a remotely-exfiltrated token cannot enroll a rogue
        // device. The refusal is real, which is why it arrives as an error here
        // rather than as a disabled button — a disabled button is not a control.
        if model.needsFreshLocalAuth {
            StatusChip(
                titleKey: "ios.crossdevice.local_auth_required",
                symbolName: "lock.badge.clock.fill",
                tint: .orange
            )
        }
        AuthErrorBanner(error: error) { Task { await model.issueCode() } }
    }
}

// MARK: - Previews

#Preview("Cross-device add") {
    let world = AuthPreviewEnvironment.healthy
    return CrossDeviceAddView(
        enrollment: world.enrollment,
        ceremony: world.crossDevice,
        now: world.now
    )
}

#Preview("Cross-device add — safety codes diverge") {
    let world = AuthPreviewEnvironment(scenario: .healthy, safetyCodesDiverge: true)
    return CrossDeviceAddView(
        enrollment: world.enrollment,
        ceremony: world.crossDevice,
        now: world.now
    )
}

#Preview("Cross-device add — offline, dark") {
    let world = AuthPreviewEnvironment.offline
    return CrossDeviceAddView(
        enrollment: world.enrollment,
        ceremony: world.crossDevice,
        now: world.now
    )
    .preferredColorScheme(.dark)
}
