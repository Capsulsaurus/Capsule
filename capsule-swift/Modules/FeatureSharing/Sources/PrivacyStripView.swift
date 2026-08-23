import CapsuleUI
import SwiftUI

// MARK: - PrivacyStripView

/// The list of what is removed when content crosses a trust boundary.
///
/// Shown **plainly**, as text, not behind a disclosure and not behind a toggle
/// that does not exist. On the share surface the strip is mandatory
/// (*Share Links — Metadata Stripping*: "There is no per-share opt-out that
/// could leak fingerprinting fields"), so the honest presentation is a
/// statement of fact; hiding it behind a switched-off control would suggest the
/// user could change it.
struct PrivacyStripView: View {
    let policy: PrivacyStripPolicy
    /// Called only where ``PrivacyStripPolicy/allowsRetention`` is true.
    ///
    /// `@MainActor @Sendable` because SwiftUI's `Binding` setter is `@Sendable`:
    /// the isolation is what lets the closure legitimately touch a main-actor
    /// view model instead of the compiler having to assume a data race.
    let setRetention: (@MainActor @Sendable (Bool) -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            ForEach(PrivacyStripField.allCases) { field in
                row(field)
            }
            footer
        }
    }

    private func row(_ field: PrivacyStripField) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
            Image(systemName: symbol(for: field))
                .foregroundStyle(.secondary)
                .frame(width: 20)
            Text(title(for: field))
            Spacer(minLength: CapsuleTheme.Spacing.small)
            Text(dispositionTitle(for: field))
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var footer: some View {
        if policy.allowsRetention, let setRetention {
            // Called, not forwarded: handing the stored `@MainActor @Sendable`
            // closure straight to `set:` makes SILGen emit an `@isolated(any)`
            // reabstraction thunk that crashes IRGen in Swift 6.3.3.
            Toggle("ios.export.retain.toggle", isOn: Binding(
                get: { policy.retainsIdentifyingMetadata },
                set: { setRetention($0) }
            ))
            Text("ios.export.retain.footer")
                .font(.footnote)
                .foregroundStyle(.secondary)
        } else {
            ScopeNote(message: "ios.share.privacy.no_opt_out")
        }
    }

    /// The right-hand column: what happens to the field *given the current
    /// policy*, so flipping the export opt-in visibly changes every row rather
    /// than silently changing behaviour behind an unchanged list.
    private func dispositionTitle(for field: PrivacyStripField) -> LocalizedStringKey {
        switch policy.effectiveDisposition(for: field) {
        case .removed: "ios.export.disposition.removed"
        case .reduced: "ios.export.disposition.reduced"
        case nil: "ios.export.disposition.retained"
        }
    }

    private func title(for field: PrivacyStripField) -> LocalizedStringKey {
        switch field {
        case .cameraSerial: "ios.export.field.camera_serial"
        case .deviceIdentifier: "ios.export.field.device_id"
        case .sessionIdentifier: "ios.export.field.session_id"
        case .location: "ios.export.field.location"
        case .contactTags: "ios.export.field.contact_tags"
        }
    }

    private func symbol(for field: PrivacyStripField) -> String {
        switch field {
        case .cameraSerial: "camera"
        case .deviceIdentifier: "iphone"
        case .sessionIdentifier: "clock.arrow.circlepath"
        case .location: "location"
        case .contactTags: "person.crop.circle"
        }
    }
}
