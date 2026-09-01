import AssetKit
import MapKit
import SwiftUI

/// The static map behind the info panel's location card.
///
/// Non-interactive on purpose: this is a *statement* about where the photograph
/// was taken, not a map to explore. Panning it inside a sheet that is itself
/// draggable puts two gestures on the same pixels, and the reader who wants to
/// explore has the Places screen.
struct AssetInfoMap: View {
    let coordinate: AssetCoordinate

    /// How much ground the frame covers. Tight enough to name a
    /// neighbourhood, wide enough that a pin never sits on a featureless tile.
    private static let span: CLLocationDistance = 800

    var body: some View {
        Map(initialPosition: .region(MKCoordinateRegion(
            center: CLLocationCoordinate2D(
                latitude: coordinate.latitude,
                longitude: coordinate.longitude
            ),
            latitudinalMeters: Self.span,
            longitudinalMeters: Self.span
        ))) {
            Marker("", coordinate: CLLocationCoordinate2D(
                latitude: coordinate.latitude,
                longitude: coordinate.longitude
            ))
        }
        .allowsHitTesting(false)
    }
}
