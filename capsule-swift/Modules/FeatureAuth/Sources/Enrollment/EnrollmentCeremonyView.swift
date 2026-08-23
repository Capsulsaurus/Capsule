import CapsuleUI
import SwiftUI

// MARK: - EnrollmentCeremonyView

/// First-device enrollment, drawn as a **named step rail**.
///
/// There is no percentage anywhere on this screen, and that is a product
/// decision rather than a stylistic one. The six steps of *Device Enrollment —
/// First-Device Enrollment* individually succeed, individually fail, and
/// individually matter: a bar at 47% cannot say "your secure element refused,
/// here is what continuing in software costs you", and it cannot say "your keys
/// are valid — only the directory upload is waiting for the server".
///
/// The screen is **not dismissable by gesture**. A ceremony abandoned halfway
/// leaves an account with no usable master key, so leaving is an explicit,
/// confirmed act rather than a downward swipe.
///
/// Entry point: ``init(enrollment:onComplete:)``, needing
/// ``FirstDeviceEnrollmentPort``.
public struct EnrollmentCeremonyView: View {
    @State private var model: EnrollmentCeremonyViewModel
    @State private var isConfirmingCancel = false
    private let onComplete: () -> Void

    public init(enrollment: any FirstDeviceEnrollmentPort, onComplete: @escaping () -> Void = {}) {
        _model = State(wrappedValue: EnrollmentCeremonyViewModel(enrollment: enrollment))
        self.onComplete = onComplete
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: "ios.enrollment.title",
                    subtitleKey: "ios.enrollment.subtitle",
                    symbolName: "key.horizontal.fill"
                )
                custody
                content
            }
        }
        .interactiveDismissDisabled()
        .task { await start() }
        .confirmationDialog(
            "ios.enrollment.cancel.confirm.title",
            isPresented: $isConfirmingCancel,
            titleVisibility: .visible
        ) {
            Button("ios.enrollment.cancel.confirm.action", role: .destructive) {
                Task { await model.cancel() }
            }
            Button("ios.common.cancel", role: .cancel) {}
        } message: {
            Text("ios.enrollment.cancel.confirm.message")
        }
    }

    /// The rail is drawn in every state except an outright failure, including
    /// before the first stage reports and after a cancellation.
    ///
    /// A spinner in those moments would be worse than the rail it replaced: the
    /// rail already says every step is pending, and a cancelled ceremony that
    /// fell back to a spinner would leave the user on a screen they cannot
    /// dismiss with no way to start again.
    @ViewBuilder
    private var content: some View {
        if case let .failed(error) = model.state {
            AuthErrorBanner(error: error) { Task { await start() } }
        } else {
            rail
            recovery
            deferredNote
            actions
        }
    }

    /// What this device will do with the classical half of its device keys, said
    /// **before** the rail starts rather than discovered halfway through.
    ///
    /// Named for what it is. Every shipping secure element software-seals the
    /// post-quantum half, so "hardware keys" would overclaim; the honest phrase
    /// is hardware-backed where the platform allows.
    private var custody: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            StatusChip(
                titleKey: model.usesSecureElement
                    ? "ios.enrollment.custody.secure_element"
                    : "ios.enrollment.custody.software",
                symbolName: model.usesSecureElement ? "lock.shield.fill" : "lock.slash.fill",
                tint: model.usesSecureElement ? .green : .orange
            )
            Text("ios.enrollment.custody.footnote")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if model.acceptedSoftwareKeyDeviation {
                StatusChip(
                    titleKey: "ios.enrollment.deviation.accepted",
                    symbolName: "exclamationmark.triangle.fill",
                    tint: .orange
                )
            }
        }
        .authCard()
    }

    private var rail: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Text("ios.enrollment.rail.header")
                .font(.headline)
            ForEach(model.rows) { row in
                EnrollmentStageRailRow(row: row)
            }
        }
    }

    @ViewBuilder
    private var deferredNote: some View {
        if !model.deferredStages.isEmpty {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
                Text("ios.enrollment.deferred.header")
                    .font(.headline)
                Text("ios.enrollment.deferred.description")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .authCard()
        }
    }

    @ViewBuilder
    private var recovery: some View {
        if let failure = model.failure {
            recovery(for: failure)
        }
    }

    @ViewBuilder
    private func recovery(for failure: EnrollmentStageFailure) -> some View {
        switch failure {
        case .hardwareKeyUnavailable:
            hardwareKeyRecovery
        case let .server(error):
            AuthErrorBanner(error: error) { Task { await model.retry() } }
        case .cancelled:
            StatusChip(
                titleKey: "ios.enrollment.failure.cancelled",
                symbolName: "hand.raised.fill",
                tint: .orange
            )
        }
    }

    /// The documented deviation, labelled as one.
    ///
    /// A secure element that refuses is rare and sometimes transient, so Retry
    /// comes first. Software keys are offered **second**, in plain words about
    /// what they cost, because *Device Enrollment — Failure Modes* calls for an
    /// actionable error rather than a dead end — and a deviation the user was
    /// not told about is not a deviation, it is a downgrade.
    private var hardwareKeyRecovery: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "ios.enrollment.failure.hardware.title",
                descriptionKey: "ios.enrollment.failure.hardware.description",
                symbolName: "exclamationmark.shield.fill"
            )
            Button("ios.enrollment.failure.hardware.retry") {
                Task { await model.retry() }
            }
            .capsuleGlassButtonStyle(prominent: true)
            .disabled(model.isRunning)
            .accessibilityLabel("ios.enrollment.failure.hardware.retry")

            Button("ios.enrollment.failure.hardware.software_keys") {
                Task { await model.continueWithSoftwareKeys() }
            }
            .buttonStyle(.bordered)
            .disabled(model.isRunning)
            .accessibilityLabel("ios.enrollment.failure.hardware.software_keys")

            Text("ios.enrollment.failure.hardware.deviation_note")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .authCard()
    }

    @ViewBuilder
    private var actions: some View {
        if model.isRunning {
            AuthLoadingView(labelKey: "ios.enrollment.running")
        }
        if model.isComplete {
            completion
        } else if !model.isRunning, model.failure == nil {
            Button("ios.enrollment.start") { Task { await model.start() } }
                .capsuleGlassButtonStyle(prominent: true)
                .accessibilityLabel("ios.enrollment.start")
        }
        Button("ios.enrollment.cancel") { isConfirmingCancel = true }
            .buttonStyle(.borderless)
            .accessibilityLabel("ios.enrollment.cancel")
    }

    private var completion: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "ios.enrollment.complete.title",
                descriptionKey: "ios.enrollment.complete.description",
                symbolName: "checkmark.seal.fill"
            )
            Button("ios.enrollment.complete.action", action: onComplete)
                .capsuleGlassButtonStyle(prominent: true)
                .accessibilityLabel("ios.enrollment.complete.action")
        }
        .authCard()
    }

    /// Ask what the device can do, then run. Two calls rather than one so the
    /// custody line is truthful from the first frame instead of changing its
    /// story once the first stage reports.
    private func start() async {
        await model.prepare()
        await model.start()
    }
}

// MARK: - Previews

#Preview("Enrollment ceremony") {
    EnrollmentCeremonyView(enrollment: AuthPreviewEnvironment.neverSignedIn.ceremony)
}

#Preview("Enrollment — hardware key refused") {
    let world = AuthPreviewEnvironment(
        scenario: .neverSignedIn,
        ceremonyBehaviour: .hardwareRefusal
    )
    return EnrollmentCeremonyView(enrollment: world.ceremony)
}

#Preview("Enrollment — server unreachable, dark") {
    let world = AuthPreviewEnvironment(
        scenario: .offline,
        ceremonyBehaviour: .serverUnreachable
    )
    return EnrollmentCeremonyView(enrollment: world.ceremony)
        .preferredColorScheme(.dark)
}
