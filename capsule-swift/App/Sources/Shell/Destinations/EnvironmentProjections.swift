import FeatureImport
import FeatureSettings
import FeatureSharing
import SwiftUI

// Projections of ports that already live on the composition root.
//
// Three feature modules gather the ports they need into one value at their own
// boundary — eleven arguments threaded through a root view that renders none of
// them is noise. Those values are *derived*, so they are computed here rather
// than stored on ``AppEnvironment``: storing them would create a second place
// the wiring can be wrong, and each costs a struct initializer to build.

extension AppEnvironment {
    /// Every port the settings tree needs.
    var settingsEnvironment: SettingsEnvironment {
        SettingsEnvironment(
            auth: auth,
            devices: devices,
            enrollment: enrollment,
            recovery: recovery,
            settings: settings,
            maintenance: maintenance,
            sync: sync,
            storage: storage,
            quota: quota,
            uploads: uploads,
            importing: importing,
            albums: albums,
            intelligence: intelligence,
            moderation: moderation,
            federation: federation,
            peering: peering,
            activeScenarioName: scenario.rawValue
        )
    }

    /// The connection probe the sharing screens share, so "offline" and "failed"
    /// stay distinguishable.
    var sharingConnectivity: SharingConnectivity {
        SharingConnectivity(sync: sync)
    }

    /// Every port the import pipeline needs.
    var importEnvironment: ImportEnvironment {
        ImportEnvironment(
            importing: importing,
            storage: storage,
            albums: albums,
            sync: sync
        )
    }
}
