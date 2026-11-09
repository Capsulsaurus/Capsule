import CapsuleMock
import CapsuleNavigation
import SwiftUI

// MARK: - AdvancedSettingsView

/// Advanced — the mock-scenario switcher, shown only in a build running a mock
/// world.
///
/// The scenario names are drawn verbatim: they are the `-mock-scenario`
/// launch-argument values a developer types, not copy, and translating them
/// would break the one-to-one match with the argument they stand for.
public struct AdvancedSettingsView: View {
    @State private var model: AdvancedSettingsModel

    public init(model: AdvancedSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: AdvancedSettingsModel(
                activeScenarioName: environment.activeScenarioName,
                launchArguments: ProcessInfo.processInfo.arguments,
                selection: MockScenarioSelection()
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.advanced.titleKey,
            phase: model.phase,
            emptyTitleKey: "app.settings.advanced.empty.title",
            emptyDescriptionKey: "app.settings.advanced.empty.description",
            retry: {},
            content: {
                scenarioSection
            }
        )
    }

    private var scenarioBinding: Binding<MockScenario> {
        Binding(
            get: { model.pickedScenario },
            set: { model.choose($0) }
        )
    }

    private var scenarioSection: some View {
        Section {
            SettingsValueRow(
                labelKey: "app.settings.advanced.scenario.active",
                value: model.activeScenarioName ?? SettingsFormat.unknown
            )
            Picker("app.settings.advanced.scenario.picker", selection: scenarioBinding) {
                ForEach(model.scenarios, id: \.self) { scenario in
                    Text(verbatim: scenario.rawValue).tag(scenario)
                }
            }
            .disabled(!model.canChooseScenario)
            .accessibilityIdentifier("settings.advanced.scenario")
            if model.appliesOnNextLaunch {
                SettingsNoteRow(textKey: "app.settings.advanced.scenario.next_launch")
            }
            if model.nextLaunchScenario != nil {
                Button("app.settings.advanced.scenario.clear") {
                    model.clearOverride()
                }
            }
        } header: {
            Text("app.settings.advanced.scenario.header")
        } footer: {
            Text("app.settings.advanced.scenario.footer")
        }
    }
}

#Preview("Advanced") {
    NavigationStack {
        AdvancedSettingsView(environment: .preview(.quarantine))
    }
}

#Preview("Advanced — Dark") {
    NavigationStack {
        AdvancedSettingsView(environment: .preview())
    }
    .preferredColorScheme(.dark)
}
