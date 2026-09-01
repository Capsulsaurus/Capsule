import CapsuleMock
import CapsuleNavigation
import Foundation
import Observation
import SwiftUI

// MARK: - AppearanceTheme

/// The three theme choices.
///
/// ``system`` is the default and is not merely "one of three": the platform
/// already knows whether the user prefers dark, and overriding it is the
/// exception a user has to ask for.
public enum AppearanceTheme: String, Sendable, Hashable, CaseIterable {
    case system
    case light
    case dark

    public var titleKey: String { "app.settings.appearance.theme.\(rawValue)" }

    /// The SwiftUI scheme to force, or `nil` to follow the system.
    public var colorScheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }
}

// MARK: - GridDensity

/// How tightly the photo grid packs.
public enum GridDensity: String, Sendable, Hashable, CaseIterable {
    case compact
    case regular
    case spacious

    public var titleKey: String { "app.settings.appearance.density.\(rawValue)" }
}

// MARK: - AppearancePreferences

/// The appearance knobs, persisted per device.
///
/// Device-local on purpose and not part of ``LibrarySettings``: a Mac and a
/// phone have different screens, and a grid density that synced would be one
/// device dictating layout to another. There is no port for the same reason —
/// nothing here crosses the network.
@MainActor
@Observable
public final class AppearancePreferences {
    private enum Key {
        static let theme = "capsule.settings.appearance.theme"
        static let density = "capsule.settings.appearance.density"
        static let reducesGlass = "capsule.settings.appearance.reduces_glass"
        static let autoplaysVideo = "capsule.settings.appearance.autoplays_video"
    }

    public var theme: AppearanceTheme {
        didSet { defaults.set(theme.rawValue, forKey: Key.theme) }
    }

    public var density: GridDensity {
        didSet { defaults.set(density.rawValue, forKey: Key.density) }
    }

    /// Whether to fall back to opaque materials.
    ///
    /// Named for what it does rather than "disable Liquid Glass": the system
    /// setting it complements is Reduce Transparency, and a user who turned
    /// that on has already answered this question.
    public var reducesGlassEffects: Bool {
        didSet { defaults.set(reducesGlassEffects, forKey: Key.reducesGlass) }
    }

    public var autoplaysVideo: Bool {
        didSet { defaults.set(autoplaysVideo, forKey: Key.autoplaysVideo) }
    }

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        theme = AppearanceTheme(rawValue: defaults.string(forKey: Key.theme) ?? "") ?? .system
        density = GridDensity(rawValue: defaults.string(forKey: Key.density) ?? "") ?? .regular
        reducesGlassEffects = defaults.bool(forKey: Key.reducesGlass)
        autoplaysVideo = defaults.object(forKey: Key.autoplaysVideo) as? Bool ?? true
    }
}

// MARK: - AppearanceSettingsModel

/// Drives the Appearance screen. Thin by design — nothing here touches a port,
/// fails, or waits, so the screen is always ``SettingsPhase/ready``.
@MainActor
@Observable
public final class AppearanceSettingsModel {
    public let preferences: AppearancePreferences
    public private(set) var phase: SettingsPhase = .ready

    public init(preferences: AppearancePreferences = AppearancePreferences()) {
        self.preferences = preferences
    }

    public var themes: [AppearanceTheme] { AppearanceTheme.allCases }
    public var densities: [GridDensity] { GridDensity.allCases }
}

// MARK: - AppearanceSettingsView

/// Appearance — theme, density, and material options.
public struct AppearanceSettingsView: View {
    @State private var model: AppearanceSettingsModel

    public init(model: AppearanceSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment _: SettingsEnvironment) {
        self.init(model: AppearanceSettingsModel())
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.appearance.titleKey,
            phase: model.phase,
            retry: {},
            content: {
                themeSection
                gridSection
                materialSection
            }
        )
    }

    private var themeSection: some View {
        Section {
            Picker("app.settings.appearance.theme.label", selection: themeBinding) {
                ForEach(model.themes, id: \.self) { theme in
                    Text(LocalizedStringKey(theme.titleKey)).tag(theme)
                }
            }
            .pickerStyle(.inline)
        } header: {
            Text("app.settings.appearance.theme.header")
        } footer: {
            Text("app.settings.appearance.theme.footer")
        }
    }

    private var gridSection: some View {
        Section {
            Picker("app.settings.appearance.density.label", selection: densityBinding) {
                ForEach(model.densities, id: \.self) { density in
                    Text(LocalizedStringKey(density.titleKey)).tag(density)
                }
            }
            .pickerStyle(.inline)
            Toggle("app.settings.appearance.autoplay.toggle", isOn: autoplayBinding)
        } header: {
            Text("app.settings.appearance.density.header")
        } footer: {
            Text("app.settings.appearance.density.footer")
        }
    }

    private var materialSection: some View {
        Section {
            Toggle("app.settings.appearance.glass.toggle", isOn: glassBinding)
        } header: {
            Text("app.settings.appearance.glass.header")
        } footer: {
            Text("app.settings.appearance.glass.footer")
        }
    }

    private var themeBinding: Binding<AppearanceTheme> {
        Binding(get: { model.preferences.theme }, set: { model.preferences.theme = $0 })
    }

    private var densityBinding: Binding<GridDensity> {
        Binding(get: { model.preferences.density }, set: { model.preferences.density = $0 })
    }

    private var glassBinding: Binding<Bool> {
        Binding(
            get: { model.preferences.reducesGlassEffects },
            set: { model.preferences.reducesGlassEffects = $0 }
        )
    }

    private var autoplayBinding: Binding<Bool> {
        Binding(
            get: { model.preferences.autoplaysVideo },
            set: { model.preferences.autoplaysVideo = $0 }
        )
    }
}

// MARK: - Preview

#Preview("Appearance") {
    NavigationStack {
        AppearanceSettingsView(environment: .preview())
    }
}

#Preview("Appearance — Dark") {
    NavigationStack {
        AppearanceSettingsView(environment: .preview())
    }
    .preferredColorScheme(.dark)
}
