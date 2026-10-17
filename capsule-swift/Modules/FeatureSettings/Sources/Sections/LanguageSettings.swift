import CapsuleMock
import CapsuleNavigation
import Foundation
import Observation
import SwiftUI

// MARK: - LanguageSettingsModel

/// Drives the Language & Region screen.
///
/// There is deliberately **no in-app language picker**. *i18n* puts the
/// resolution on the platform — "native clients use their platform's own ICU
/// machinery" — so the language this app draws in is the one the OS resolved
/// from the user's ordered preferences. A second, app-local override would be a
/// second answer to a question the system has already answered, and the two
/// would disagree the first time a user changed one of them.
///
/// What the screen *can* honestly do is show which locale actually resolved,
/// which of the catalog's locales are shipping, and where to change it.
@MainActor
@Observable
public final class LanguageSettingsModel {
    public private(set) var phase: SettingsPhase = .ready

    /// The catalog's locale set: English, the source, plus twelve translations.
    ///
    /// Identifiers rather than names — the names come from the OS, localized
    /// into the reader's own language, which is both correct and something no
    /// translator should have to maintain.
    public static let catalogLocaleIdentifiers = [
        "en", "zh-Hans", "zh-Hant", "ja", "ko", "fr",
        "de", "es", "pt-BR", "it", "ru", "hi", "ar",
    ]

    private let bundle: Bundle
    private let locale: Locale

    public init(bundle: Bundle = .main, locale: Locale = .current) {
        self.bundle = bundle
        self.locale = locale
    }

    /// The localization the bundle actually resolved to.
    public var resolvedLocalization: String {
        bundle.preferredLocalizations.first ?? "en"
    }

    /// The user's ordered language preferences, as the OS reports them.
    public var preferredLanguages: [String] {
        Array(Locale.preferredLanguages.prefix(4))
    }

    /// The region, which is a separate axis from language and formats dates,
    /// numbers, and byte counts on this screen and every other.
    public var regionIdentifier: String {
        locale.region?.identifier ?? SettingsFormat.unknown
    }

    /// Whether the resolved language lays out right-to-left.
    public var isRightToLeft: Bool {
        Locale.Language(identifier: resolvedLocalization).characterDirection == .rightToLeft
    }

    /// The localizations this build actually contains, which is not the same as
    /// the catalog's set while the rollout is in progress.
    public var shippingLocalizations: [String] {
        bundle.localizations.filter { $0 != "Base" }.sorted()
    }

    /// A locale identifier's name in the reader's own language.
    public func displayName(for identifier: String) -> String {
        locale.localizedString(forIdentifier: identifier) ?? identifier
    }

    /// Whether a catalog locale is present in this build.
    public func isShipping(_ identifier: String) -> Bool {
        shippingLocalizations.contains(identifier)
    }
}

// MARK: - LanguageSettingsView

/// Language & Region — what resolved, what ships, and where to change it.
public struct LanguageSettingsView: View {
    @State private var model: LanguageSettingsModel

    public init(model: LanguageSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment _: SettingsEnvironment) {
        self.init(model: LanguageSettingsModel())
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.language.titleKey,
            phase: model.phase,
            retry: {},
            content: {
                resolvedSection
                preferencesSection
                catalogSection
            }
        )
    }

    private var resolvedSection: some View {
        Section {
            SettingsValueRow(
                labelKey: "app.settings.language.resolved",
                value: model.displayName(for: model.resolvedLocalization)
            )
            SettingsValueRow(
                labelKey: "app.settings.language.region",
                value: model.regionIdentifier
            )
            SettingsStatusRow(
                labelKey: "app.settings.language.direction",
                statusKey: model.isRightToLeft
                    ? "app.settings.language.direction.rtl"
                    : "app.settings.language.direction.ltr",
                tone: .neutral
            )
        } header: {
            Text("app.settings.language.resolved.header")
        } footer: {
            Text("app.settings.language.resolved.footer")
        }
    }

    private var preferencesSection: some View {
        Section {
            ForEach(model.preferredLanguages, id: \.self) { identifier in
                SettingsValueRow(
                    labelKey: "app.settings.language.preferred.entry",
                    value: model.displayName(for: identifier)
                )
            }
        } header: {
            Text("app.settings.language.preferred.header")
        } footer: {
            Text("app.settings.language.preferred.footer")
        }
    }

    private var catalogSection: some View {
        Section {
            ForEach(LanguageSettingsModel.catalogLocaleIdentifiers, id: \.self) { identifier in
                SettingsStatusRow(
                    labelKey: "app.settings.language.catalog.entry",
                    statusKey: model.isShipping(identifier)
                        ? "app.settings.language.catalog.shipping"
                        : "app.settings.language.catalog.pending",
                    tone: model.isShipping(identifier) ? .positive : .neutral
                )
                SettingsValueRow(
                    labelKey: "app.settings.language.catalog.locale",
                    value: model.displayName(for: identifier)
                )
            }
        } header: {
            Text("app.settings.language.catalog.header")
        } footer: {
            Text("app.settings.language.catalog.footer")
        }
    }
}

// MARK: - Preview

#Preview("Language & Region") {
    NavigationStack {
        LanguageSettingsView(environment: .preview())
    }
}
