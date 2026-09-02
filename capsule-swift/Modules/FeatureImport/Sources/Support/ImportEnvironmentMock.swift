import CapsuleMock
import Foundation

// MARK: - Mock composition

public extension ImportEnvironment {
    /// Wire the import pipeline to a mock world.
    ///
    /// Building it from a ``MockEnvironment`` rather than from loose stubs is
    /// deliberate: a scenario is coherent across ports by construction, so the
    /// space meter and the plan cannot disagree about how big the library is.
    /// The clock comes from the same configuration, so a preview and a test see
    /// the same "now".
    init(mock: MockEnvironment, platform: ImportPlatform = .current) {
        self.init(
            importing: mock.importing,
            storage: mock.storage,
            albums: mock.albums,
            sync: mock.sync,
            clock: .fixed(epochSeconds: mock.configuration.clock.now.epochSeconds),
            platform: platform
        )
    }

    /// An import environment for one named mock world.
    static func preview(
        _ scenario: MockScenario = .healthy,
        platform: ImportPlatform = .current
    ) -> ImportEnvironment {
        ImportEnvironment(mock: MockEnvironment(scenario: scenario), platform: platform)
    }
}
