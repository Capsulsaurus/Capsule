import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - ProtocolUpgradeRequiredView

/// The `426` hard stop.
///
/// Compatibility is gated **once, up front**, by the universal protocol
/// handshake — there is no per-request capability negotiation (*Upload Protocol
/// — Protocol Versioning*), and the client recovery for
/// `error.protocol.version_unsupported` is ``RecoveryAction/abortWithUpgrade``:
/// *"Stop and tell the user to update. There is no negotiation: a client either
/// speaks a protocol version the server accepts, or it does not upload."*
///
/// So this screen offers exactly one way forward and **never a downgrade**.
/// There is deliberately no "continue anyway", no "use an older protocol", and
/// no retry: each of those would either loop forever against a server that will
/// never accept this build, or invite a client into a version window the
/// handshake exists to keep it out of.
///
/// Route entry point. Ports required: **none** — the screen states a fact the
/// handshake already established, and reading a port to restate it would only
/// produce another refusal.
public struct ProtocolUpgradeRequiredView: View {
    /// What "get the new version" means on this deployment. Absent for a build
    /// with no update channel, in which case the screen explains rather than
    /// offering a button that does nothing.
    private let updateAction: (@MainActor () -> Void)?

    /// The hero glyph's size, scaled by the user's text size.
    ///
    /// `@ScaledMetric` rather than a bare point size: a fixed `.system(size:)`
    /// does not grow with Dynamic Type at all, which the accessibility audit
    /// reports as "Dynamic Type font sizes are unsupported" — and which means a
    /// user who needs larger text gets a normal-sized icon above it. A text
    /// style would be the simpler answer, but none of them is 56 points, and
    /// this glyph is the screen's whole visual anchor.
    @ScaledMetric(relativeTo: .largeTitle) private var heroSize: CGFloat = 56

    public init(updateAction: (@MainActor () -> Void)? = nil) {
        self.updateAction = updateAction
    }

    public var body: some View {
        VStack(spacing: CapsuleTheme.Spacing.large) {
            Image(systemName: "exclamationmark.octagon.fill")
                .font(.system(size: heroSize))
                .foregroundStyle(.red)
                .accessibilityHidden(true)
            Text("app.transfer.upgrade.title")
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
            // The server's own code, localized by the catalog. The English
            // detail message never reaches this screen.
            Text(LocalizedStringKey(ErrorCode.protocolVersionUnsupported.rawValue))
                .font(.body)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            Text("app.transfer.upgrade.explanation")
                .font(.footnote)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            reassurance
            if let updateAction {
                Button("app.transfer.recovery.update_app", action: updateAction)
                    .capsuleGlassButtonStyle(prominent: true)
            } else {
                Text("app.transfer.upgrade.no_channel")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(CapsuleTheme.Spacing.xLarge)
        .frame(maxWidth: 480)
        .navigationTitle("app.transfer.upgrade.title")
    }

    /// The screen has to be honest that nothing was lost. A hard stop on the
    /// upload path leaves the library readable and every local copy intact —
    /// saying so is what stops a version mismatch reading as data loss.
    private var reassurance: some View {
        Label("app.transfer.upgrade.data_safe", systemImage: "lock.shield")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.leading)
    }
}

// MARK: - Previews

#Preview("Update available") {
    NavigationStack {
        ProtocolUpgradeRequiredView(updateAction: {})
    }
}

#Preview("No update channel") {
    NavigationStack {
        ProtocolUpgradeRequiredView()
    }
    .preferredColorScheme(.dark)
}
