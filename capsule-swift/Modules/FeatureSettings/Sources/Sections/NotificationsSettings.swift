import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import CapsulePorts
import Observation
import SwiftUI

// MARK: - NotificationCategory

/// The alerts this app raises, and what each one is for.
///
/// Only one of them has a switch. That is not an omission: the other three are
/// raised by contracts that say the user must be told — a quota that has stopped
/// accepting uploads, a held item awaiting a human decision, a recovery secret
/// that has not been checked in months — and a settings screen that offered to
/// silence them would be offering to hide the fact that something needs doing.
/// They are listed so the list is complete, with the reason attached.
public enum NotificationCategory: String, Sendable, Hashable, CaseIterable {
    /// The two-week staleness warning. Snoozeable and switchable.
    case staleness
    /// Quota thresholds crossed.
    case quota
    /// Something was quarantined and needs a decision.
    case quarantine
    /// The recovery-verification cadence came due.
    case recovery

    public var titleKey: String { "ios.settings.notifications.category.\(rawValue)" }
    public var detailKey: String { "ios.settings.notifications.detail.\(rawValue)" }

    /// Whether the user can turn this category off entirely.
    public var isSwitchable: Bool { self == .staleness }
}

// MARK: - NotificationsSettingsModel

/// Drives the Notifications screen.
@MainActor
@Observable
public final class NotificationsSettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var stalenessWarningEnabled = true
    public private(set) var isWorking = false

    private let settings: any SettingsPort
    private let connectivity: SettingsConnectivity

    public init(settings: any SettingsPort, connectivity: SettingsConnectivity) {
        self.settings = settings
        self.connectivity = connectivity
    }

    public var categories: [NotificationCategory] { NotificationCategory.allCases }

    public func load() async {
        phase = .loading
        do {
            stalenessWarningEnabled = try await settings.settings().stalenessNotificationEnabled
            phase = .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Turn the staleness warning off.
    ///
    /// Opts out of the **warning** only; auto sync itself is unaffected. The
    /// two are deliberately separate, because a user who dismissed a notice has
    /// not asked to stop syncing, and conflating them would quietly strand
    /// their library.
    public func setStalenessWarningEnabled(_ enabled: Bool) async {
        isWorking = true
        defer { isWorking = false }
        do {
            var current = try await settings.settings()
            current.stalenessNotificationEnabled = enabled
            try await settings.update(current)
            stalenessWarningEnabled = enabled
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }
}

// MARK: - NotificationsSettingsView

/// Notifications — which alerts are raised, and the one that can be silenced.
public struct NotificationsSettingsView: View {
    @State private var model: NotificationsSettingsModel

    public init(model: NotificationsSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: NotificationsSettingsModel(
                settings: environment.settings,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.notifications.titleKey,
            phase: model.phase,
            retry: { await model.load() },
            content: {
                ForEach(model.categories, id: \.self) { category in
                    categorySection(category)
                }
            }
        )
        .task { await model.load() }
    }

    private func categorySection(_ category: NotificationCategory) -> some View {
        Section {
            if category.isSwitchable {
                Toggle(LocalizedStringKey(category.titleKey), isOn: stalenessBinding)
                    .disabled(model.isWorking)
            } else {
                SettingsStatusRow(
                    labelKey: category.titleKey,
                    statusKey: "ios.settings.notifications.always_on",
                    tone: .neutral
                )
            }
        } footer: {
            Text(LocalizedStringKey(category.detailKey))
        }
    }

    private var stalenessBinding: Binding<Bool> {
        Binding(
            get: { model.stalenessWarningEnabled },
            set: { newValue in Task { await model.setStalenessWarningEnabled(newValue) } }
        )
    }
}

// MARK: - Preview

#Preview("Notifications") {
    NavigationStack {
        NotificationsSettingsView(environment: .preview())
    }
}
