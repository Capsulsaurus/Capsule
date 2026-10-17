import CapsuleNavigation
import FeatureSettings
import FeatureSharing
import SwiftUI

extension RouteDestination {
    /// One settings screen, or the index when the route names none.
    ///
    /// ``SettingsSection/default`` is what a settings route carries when it has
    /// no particular screen in mind — and it is also the Settings section's
    /// landing route — so it resolves to the index rather than to a screen.
    /// Every other section resolves to its own screen, which is what makes a
    /// link straight to, say, Language work.
    @ViewBuilder
    func settingsDestination(_ section: SettingsSection) -> some View {
        if section == .default {
            SettingsIndexView(environment: environment)
        } else {
            settingsScreen(section, environment)
        }
    }

    /// Scheduled integrity and housekeeping jobs.
    ///
    /// The route's optional task kind is not forwarded: the screen already lists
    /// every task with its own state and confirmation, so "open Maintenance at
    /// the scrub row" is a scroll position rather than a different screen. The
    /// kind stays on the route because a notification tap addresses it.
    var maintenanceDestination: some View {
        settingsScreen(.maintenance, environment)
    }
}

// MARK: - SettingsIndexView

/// The settings index: every screen, grouped, in catalog order.
///
/// The rows come from ``SettingsRootCatalog`` rather than from a list written
/// here. That is deliberate — the catalog also decides the Mac tab order and
/// what a link resolver walks, and a second hand-written list is how a screen
/// ends up reachable on one surface and invisible on another.
/// `SettingsRootCatalog.coversEverySection` is the predicate that pins it.
///
/// Rows push a ``SettingsSection``, not a `Route`: the Settings section's own
/// landing route *is* this index, so pushing `Route.settings` from here would
/// resolve straight back to it.
struct SettingsIndexView: View {
    let environment: AppEnvironment

    var body: some View {
        List {
            ForEach(SettingsRootCatalog.groups) { group in
                Section(LocalizedStringKey(group.titleKey)) {
                    ForEach(group.sections, id: \.self) { section in
                        row(for: section)
                    }
                }
            }
        }
        .navigationTitle(LocalizedStringKey(SidebarItem.settings.titleKey))
        .navigationDestination(for: SettingsSection.self) { section in
            settingsScreen(section, environment)
        }
    }

    private func row(for section: SettingsSection) -> some View {
        NavigationLink(value: section) {
            Label(
                LocalizedStringKey(section.titleKey),
                systemImage: SettingsRootCatalog.symbol(for: section)
            )
        }
        .accessibilityIdentifier("settings.section.\(section.rawValue)")
    }
}

// MARK: - The section screens

// swiftlint:disable cyclomatic_complexity

/// One settings screen, chosen by section.
///
/// A free function rather than a view of its own, for two reasons. It is called
/// from both the index and ``RouteDestination``, and neither owns it; and being
/// inline keeps the whole settings decision inside one view tree, so the test
/// that walks `RouteDestination` for placeholders can actually see through it.
///
/// Exhaustive over ``SettingsSection`` for the same reason ``RouteDestination``
/// is exhaustive over `Route`: a screen added to the closed set without a
/// destination must fail to build, not show up as an empty row.
///
/// One branch per section is the point of the switch; the complexity score it
/// earns is the cost of the guarantee, not a smell to be refactored away.
@MainActor @ViewBuilder
func settingsScreen(_ section: SettingsSection, _ environment: AppEnvironment) -> some View {
    switch section {
    case .account: AccountSettingsView(environment: environment.settingsEnvironment)
    case .server: ServerSettingsView(environment: environment.settingsEnvironment)
    case .security: SecuritySettingsView(environment: environment.settingsEnvironment)
    case .keysAndDevices: KeysAndDevicesSettingsView(environment: environment.settingsEnvironment)
    case .backupAndRecovery: BackupAndRecoverySettingsView(environment: environment.settingsEnvironment)
    case .sync: SyncSettingsView(environment: environment.settingsEnvironment)
    case .storage: StorageSettingsView(environment: environment.settingsEnvironment)
    case .importAndScopes: ImportAndScopesSettingsView(environment: environment.settingsEnvironment)
    case .aiAndModels: AIAndModelsSettingsView(environment: environment.settingsEnvironment)
    case .appearance: AppearanceSettingsView(environment: environment.settingsEnvironment)
    case .language: LanguageSettingsView(environment: environment.settingsEnvironment)
    case .notifications: NotificationsSettingsView(environment: environment.settingsEnvironment)
    case .maintenance: MaintenanceSettingsView(environment: environment.settingsEnvironment)
    case .moderation:
        ModerationView(
            model: ModerationViewModel(
                moderation: environment.moderation,
                records: environment.moderationRecords,
                originPolicy: environment.untrustedOriginPolicy,
                connectivity: environment.sharingConnectivity
            )
        )
    // The app owns the diagnostics consent and bug-report screen: it wires
    // MetricKit and the export sheet, neither of which a feature module can
    // reach. Linking to it beats shipping a second consent screen.
    case .diagnostics:
        SettingsView(
            consentStore: environment.consentStore,
            diagnostics: environment.diagnostics
        )
    // Peer budgets and breakers, the escape hatches, and the acknowledgements
    // have no screen yet.
    case .federation, .advanced, .about:
        RouteScaffold(
            titleKey: section.titleKey,
            systemImage: SettingsRootCatalog.symbol(for: section)
        )
    }
}

// swiftlint:enable cyclomatic_complexity
