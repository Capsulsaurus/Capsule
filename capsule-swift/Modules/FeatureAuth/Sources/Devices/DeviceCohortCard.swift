import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - PlatformTag presentation

extension PlatformTag {
    /// The rail symbol for this platform. Paired with ``nameKey`` everywhere it
    /// appears, so the glyph never carries the meaning on its own.
    var symbolName: String {
        switch self {
        case .ios: "iphone.gen3"
        case .android: "candybarphone"
        case .macos: "laptopcomputer"
        case .windows: "pc"
        case .linux: "desktopcomputer"
        case .unknown: "questionmark.square.dashed"
        }
    }

    /// The catalog key naming this platform.
    var nameKey: String {
        switch self {
        case .ios: "app.devices.platform.ios"
        case .android: "app.devices.platform.android"
        case .macos: "app.devices.platform.macos"
        case .windows: "app.devices.platform.windows"
        case .linux: "app.devices.platform.linux"
        case .unknown: "app.devices.platform.unknown"
        }
    }
}

// MARK: - DeviceCohortCard

/// One physical device's worth of ledger rows.
///
/// The copy here **asserts, it does not litigate**. A cohort that has enrolled
/// more than once reads "a device you have used before" as a statement of fact,
/// with no "is this you?" toggle beside it — a user cannot adjudicate a hash,
/// the value is advisory and unverifiable by construction, and offering a toggle
/// would invite them to correct something that drives no decision. The dispute
/// path is the support report at the foot of the card, which is a real action
/// with a real recipient.
///
/// A revoked row stays on the card. Its key is in the directory forever so
/// everything it ever signed remains verifiable, and a user auditing their
/// account should see the same history the cryptography does.
struct DeviceCohortCard: View {
    let group: DeviceCohortGroup
    let now: CapsuleTimestamp
    let isRevoking: Bool
    let revokeDevice: (DeviceID) -> Void
    let revokeSession: (SessionID) -> Void
    let buildSupportBundle: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            header
            devices
            sessions
            supportBundleButton
        }
        .authCard()
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Label {
                Text(verbatim: group.representativeDevice?.model ?? "")
                    .font(.title3.weight(.semibold))
            } icon: {
                Image(systemName: group.representativeDevice?.platform.symbolName ?? "questionmark.square.dashed")
                    .font(.title3)
            }
            if group.cohortHash == nil {
                Text("app.devices.cohort.ungrouped")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if group.containsCurrentDevice {
                StatusChip(
                    titleKey: "app.devices.device.current",
                    symbolName: "checkmark.circle.fill",
                    tint: .green
                )
            }
            if group.isPreviouslySeen {
                StatusChip(
                    titleKey: "app.devices.cohort.assertion",
                    symbolName: "clock.arrow.circlepath",
                    tint: .secondary
                )
            }
            Text("app.devices.cohort.explanation")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var devices: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Text("app.devices.device.header")
                .font(.headline)
            ForEach(group.devices) { device in
                deviceRow(device)
            }
        }
    }

    private func deviceRow(_ device: DeviceRecord) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Label {
                Text(LocalizedStringKey(device.platform.nameKey))
                    .font(.subheadline.weight(.medium))
            } icon: {
                Image(systemName: device.platform.symbolName)
            }
            AuthLabeledDate(labelKey: "app.devices.device.first_seen", date: device.firstSeen.date)
            AuthLabeledDate(labelKey: "app.devices.device.last_seen", date: device.lastSeen.date)
            if device.isActive {
                Button("app.devices.device.revoke", role: .destructive) { revokeDevice(device.id) }
                    .buttonStyle(.bordered)
                    .disabled(isRevoking)
                    .accessibilityLabel("app.devices.device.revoke")
            } else {
                StatusChip(
                    titleKey: "app.devices.device.revoked",
                    symbolName: "nosign",
                    tint: .secondary
                )
            }
        }
        .authInnerCard()
    }

    @ViewBuilder
    private var sessions: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Text("app.devices.session.header")
                .font(.headline)
            if group.sessions.isEmpty {
                Text("app.devices.session.empty")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                ForEach(group.sessions) { session in
                    sessionRow(session)
                }
            }
        }
    }

    private func sessionRow(_ session: SessionRecord) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            sessionStatus(session)
            AuthLabeledDate(labelKey: "app.devices.session.created", date: session.createdAt.date)
            AuthLabeledDate(labelKey: "app.devices.session.last_used", date: session.lastUsedAt.date)
            // Whichever expiry bites first. The sliding window refreshes on use;
            // the hard ceiling does not, because its job is to bound an
            // exfiltrated token's life regardless of how busy the thief is.
            AuthLabeledDate(labelKey: "app.devices.session.expires", date: session.effectiveExpiry.date)
            if session.isLive(at: now) {
                Button("app.devices.session.revoke", role: .destructive) { revokeSession(session.id) }
                    .buttonStyle(.bordered)
                    .disabled(isRevoking)
                    .accessibilityLabel("app.devices.session.revoke")
            }
        }
        .authInnerCard()
    }

    @ViewBuilder
    private func sessionStatus(_ session: SessionRecord) -> some View {
        if session.revokedAt != nil {
            StatusChip(titleKey: "app.devices.session.revoked", symbolName: "nosign", tint: .secondary)
        } else if !session.isLive(at: now) {
            StatusChip(
                titleKey: "app.devices.session.expired",
                symbolName: "clock.badge.xmark.fill",
                tint: .orange
            )
        } else if session.isCurrent {
            StatusChip(
                titleKey: "app.devices.session.current",
                symbolName: "checkmark.circle.fill",
                tint: .green
            )
        } else {
            StatusChip(titleKey: "app.devices.session.live", symbolName: "circle.fill", tint: .green)
        }
    }

    @ViewBuilder
    private var supportBundleButton: some View {
        if let hash = group.cohortHash {
            Button("app.devices.support_bundle", systemImage: "doc.badge.gearshape") {
                buildSupportBundle(hash)
            }
            .buttonStyle(.bordered)
            .accessibilityLabel("app.devices.support_bundle")
        }
    }
}
