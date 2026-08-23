import CapsuleDomain
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - DevicesAndSessionsView

/// The session ledger, grouped by device cohort.
///
/// A flat ledger has a legibility problem this screen exists to fix: device keys
/// are hardware-bound and non-exportable, so reinstalling the app enrolls a
/// **new** device id by design, and one phone accumulates several rows over its
/// life. Ungrouped, they read as several strangers.
///
/// The two revocations are deliberately **not** two similar buttons.
/// *Authentication — Explicit Revocation* makes the asymmetry load-bearing:
/// revoking one session is authenticated by any active session, while revoking
/// every session requires proof of master-key possession. An attacker holding a
/// stolen token can therefore end that token's session and nothing more, and the
/// screen says which is which rather than leaving it to placement.
///
/// Entry point: ``init(devices:now:)``, needing ``DevicePort``.
public struct DevicesAndSessionsView: View {
    @State private var model: DevicesAndSessionsViewModel
    @State private var isConfirmingRevokeAll = false

    public init(devices: any DevicePort, now: (@Sendable () -> CapsuleTimestamp)? = nil) {
        if let now {
            _model = State(wrappedValue: DevicesAndSessionsViewModel(devices: devices, now: now))
        } else {
            _model = State(wrappedValue: DevicesAndSessionsViewModel(devices: devices))
        }
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: "ios.devices.title",
                    subtitleKey: "ios.devices.subtitle",
                    symbolName: "laptopcomputer.and.iphone"
                )
                content
            }
        }
        .task { await model.load() }
        .confirmationDialog(
            "ios.devices.revoke_all.confirm.title",
            isPresented: $isConfirmingRevokeAll,
            titleVisibility: .visible
        ) {
            Button("ios.devices.revoke_all.confirm.action", role: .destructive) {
                Task { await model.revokeAllSessions() }
            }
            Button("ios.common.cancel", role: .cancel) {}
        } message: {
            Text("ios.devices.revoke_all.confirm.message")
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.state {
        case .idle, .loading:
            AuthLoadingView(labelKey: "ios.devices.loading")
        case let .failed(error):
            failure(error)
        case .empty:
            ContentUnavailableView(
                "ios.devices.empty.title",
                systemImage: "laptopcomputer.slash",
                description: Text("ios.devices.empty.description")
            )
        case .ready:
            ledger
        }
    }

    /// A rejected or demanded master-key proof is a different conversation from
    /// an ordinary error, so it gets its own sentence above the banner rather
    /// than being flattened into "something went wrong".
    @ViewBuilder
    private func failure(_ error: AuthPresentableError) -> some View {
        if model.needsMasterKeyProof {
            StatusChip(
                titleKey: "ios.devices.proof_required",
                symbolName: "key.slash.fill",
                tint: .orange
            )
        }
        AuthErrorBanner(error: error) { Task { await model.load() } }
    }

    private var ledger: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
            if model.isRevoking {
                AuthLoadingView(labelKey: "ios.devices.revoking")
            }
            ForEach(model.groups) { group in
                cohortCard(group)
            }
            supportBundleResult
            revokeAll
        }
    }

    private func cohortCard(_ group: DeviceCohortGroup) -> some View {
        DeviceCohortCard(
            group: group,
            now: model.currentInstant,
            isRevoking: model.isRevoking,
            revokeDevice: { identifier in Task { await model.revokeDevice(identifier) } },
            revokeSession: { identifier in Task { await model.revokeSession(identifier) } },
            buildSupportBundle: { hash in Task { await model.buildSupportBundle(for: hash) } }
        )
    }

    @ViewBuilder
    private var supportBundleResult: some View {
        if let bundle = model.supportBundle {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
                AuthSectionHeader(
                    titleKey: "ios.devices.support_bundle.ready.title",
                    descriptionKey: "ios.devices.support_bundle.ready.description",
                    symbolName: "doc.badge.gearshape"
                )
                AuthCodeValue(
                    labelKey: "ios.devices.support_bundle.hash",
                    code: ChunkedCodeFormatter.chunked(bundle.cohortHash)
                )
                AuthLabeledDate(
                    labelKey: "ios.devices.support_bundle.first_seen",
                    date: bundle.firstSeen.date
                )
                Button("ios.devices.support_bundle.dismiss") { model.dismissSupportBundle() }
                    .buttonStyle(.bordered)
                    .accessibilityLabel("ios.devices.support_bundle.dismiss")
            }
            .authCard()
        }
    }

    /// The nuclear option, labelled as one.
    ///
    /// The proof requirement is stated on the screen rather than discovered when
    /// the request fails: a user about to sign every one of their devices out
    /// should know in advance that they will be asked to prove they hold the
    /// master key, and a user who cannot should learn it before they try.
    private var revokeAll: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Button("ios.devices.revoke_all", role: .destructive) { isConfirmingRevokeAll = true }
                .buttonStyle(.bordered)
                .disabled(model.isRevoking)
                .accessibilityLabel("ios.devices.revoke_all")
            if model.revokeAllRequiresMasterKeyProof {
                Text("ios.devices.revoke_all.proof_note")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .authCard()
    }
}

// MARK: - Previews

#Preview("Devices and sessions") {
    let world = AuthPreviewEnvironment.healthy
    return DevicesAndSessionsView(devices: world.devices, now: world.now)
}

#Preview("Devices and sessions — dark") {
    let world = AuthPreviewEnvironment.healthy
    return DevicesAndSessionsView(devices: world.devices, now: world.now)
        .preferredColorScheme(.dark)
}

#Preview("Devices and sessions — offline") {
    let world = AuthPreviewEnvironment.offline
    return DevicesAndSessionsView(devices: world.devices, now: world.now)
}
