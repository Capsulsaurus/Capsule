import CapsuleUI
import SwiftUI

// MARK: - AuthErrorBanner

/// The failure state, in the one shape every screen in this module uses.
///
/// The message is looked up by the error's catalog key, so a `rate_limited`
/// reads as a rate limit and an `invalid_credentials` reads as a bad
/// credential, in the user's language, without any screen writing copy of its
/// own (*i18n — Server Error Codes*).
///
/// The symbol and the text carry the meaning together. Colour is applied on top
/// of both and is never the only signal, because a red banner and a grey banner
/// are the same banner to a substantial number of people.
public struct AuthErrorBanner: View {
    private let error: AuthPresentableError
    private let retry: (() -> Void)?

    public init(error: AuthPresentableError, retry: (() -> Void)? = nil) {
        self.error = error
        self.retry = retry
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Label {
                Text(titleKey)
                    .font(.headline)
                    .fixedSize(horizontal: false, vertical: true)
            } icon: {
                Image(systemName: symbolName)
            }
            Text(LocalizedStringKey(error.messageKey))
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if error.isOffline {
                Text("app.auth.common.offline.message")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let retry, error.isRetryable {
                Button("app.auth.common.retry", action: retry)
                    .buttonStyle(.bordered)
                    .accessibilityLabel("app.auth.common.retry")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(CapsuleTheme.Spacing.medium)
        .background(background, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.medium))
        .accessibilityElement(children: .contain)
    }

    private var titleKey: LocalizedStringKey {
        switch error.kind {
        case .temporarilyUnavailable: "app.auth.common.offline.title"
        case .actionable: "app.auth.common.error.title"
        case .upgradeRequired: "app.auth.common.upgrade.title"
        case .defect: "app.auth.common.defect.title"
        }
    }

    private var symbolName: String {
        switch error.kind {
        case .temporarilyUnavailable: "wifi.exclamationmark"
        case .actionable: "exclamationmark.triangle.fill"
        case .upgradeRequired: "arrow.up.circle.fill"
        case .defect: "ladybug.fill"
        }
    }

    private var background: some ShapeStyle {
        error.kind == .temporarilyUnavailable ? AnyShapeStyle(.quaternary) : AnyShapeStyle(.tertiary)
    }
}

// MARK: - AuthLoadingView

/// The loading state. Labelled, because an unlabelled spinner tells a
/// screen-reader user nothing at all.
public struct AuthLoadingView: View {
    private let labelKey: LocalizedStringKey

    public init(labelKey: LocalizedStringKey = "app.auth.common.loading") {
        self.labelKey = labelKey
    }

    public var body: some View {
        HStack(spacing: CapsuleTheme.Spacing.small) {
            ProgressView()
            Text(labelKey)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .center)
        .padding(.vertical, CapsuleTheme.Spacing.large)
        .accessibilityElement(children: .combine)
    }
}

// MARK: - StatusChip

/// A short status, always as symbol **and** text.
///
/// Used for stage status in the enrollment rail, session liveness in the
/// ledger, and share validity in the restore flow. The accessibility audit
/// requires that colour never be the only signal, so this type has no
/// text-free configuration to misuse.
public struct StatusChip: View {
    private let titleKey: LocalizedStringKey
    private let symbolName: String
    private let tint: Color

    public init(titleKey: LocalizedStringKey, symbolName: String, tint: Color) {
        self.titleKey = titleKey
        self.symbolName = symbolName
        self.tint = tint
    }

    public var body: some View {
        Label {
            Text(titleKey)
        } icon: {
            Image(systemName: symbolName)
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(tint)
        .labelStyle(.titleAndIcon)
        .accessibilityElement(children: .combine)
    }
}
