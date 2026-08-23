import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - SafetyCodeCheckView

/// The channel-verification step, on the issuing device.
///
/// This is the MITM defence, not the enrollment code. The code is single-use,
/// ten-minute, and rate-limited, which is exactly why a transcribable digit
/// fallback can sit beside the QR without weakening anything — channel integrity
/// rests here instead. The safety code is derived from the channel transcript,
/// so a relay in the middle produces two *different* codes and the comparison
/// catches it.
///
/// The client's whole job is to make that human comparison failure-resistant:
/// identical chunking on both devices (one ``ChunkedCodeFormatter``, so they
/// cannot drift apart), each device's model and short key fingerprint beside the
/// code, and an acknowledgement that covers **both** halves at once. Confirming
/// digits without confirming the device is what a relay swap counts on.
///
/// Nothing here advances on its own, and the destructive-looking button is the
/// safe one: a mismatch is the exit, never a missed default.
struct SafetyCodeCheckView: View {
    let safetyCheck: SafetyCheck
    let safetyCodeDisplay: String
    @Binding var hasAcknowledged: Bool
    let canConfirm: Bool
    let confirm: () -> Void
    let abort: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
            AuthSectionHeader(
                titleKey: "app.crossdevice.safety.title",
                descriptionKey: "app.crossdevice.safety.description",
                symbolName: "checkmark.shield.fill"
            )
            code
            devices
            acknowledgement
            actions
        }
    }

    private var code: some View {
        AuthCodeValue(
            labelKey: "app.crossdevice.safety.code",
            code: safetyCodeDisplay,
            font: .title2.monospaced().weight(.semibold)
        )
        .authCard()
    }

    private var devices: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            deviceCard(
                titleKey: "app.crossdevice.safety.this_device",
                identity: safetyCheck.localDevice
            )
            deviceCard(
                titleKey: "app.crossdevice.safety.new_device",
                identity: safetyCheck.remoteDevice
            )
        }
    }

    private func deviceCard(titleKey: LocalizedStringKey, identity: DeviceIdentity) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Text(titleKey)
                .font(.headline)
            Label {
                Text(verbatim: identity.model)
                    .font(.callout)
            } icon: {
                Image(systemName: identity.platform.symbolName)
            }
            AuthCodeValue(
                labelKey: "app.crossdevice.safety.fingerprint",
                code: ChunkedCodeFormatter.chunked(identity.keyFingerprint),
                font: .callout.monospaced()
            )
        }
        .authInnerCard()
    }

    /// One acknowledgement covering both halves, because they fail together.
    private var acknowledgement: some View {
        Toggle("app.crossdevice.safety.acknowledge", isOn: $hasAcknowledged)
            .accessibilityLabel("app.crossdevice.safety.acknowledge")
    }

    private var actions: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Button("app.crossdevice.safety.confirm", action: confirm)
                .capsuleGlassButtonStyle(prominent: true)
                .disabled(!canConfirm)
                .accessibilityLabel("app.crossdevice.safety.confirm")

            Button("app.crossdevice.safety.mismatch", role: .destructive, action: abort)
                .buttonStyle(.bordered)
                .accessibilityLabel("app.crossdevice.safety.mismatch")

            Text("app.crossdevice.safety.mismatch_note")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}
