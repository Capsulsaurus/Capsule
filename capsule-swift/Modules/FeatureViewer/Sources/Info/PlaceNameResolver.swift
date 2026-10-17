import AssetKit
import CapsuleFoundation
import CoreLocation
import Foundation

/// Turns a coordinate into the name of the place it is in.
///
/// A protocol so the network call is substitutable — and so the *default* is
/// substitutable too. See ``PlaceNamePreference`` for why this is opt-in.
public protocol PlaceNameResolver: Sendable {
    /// The locality for a coordinate, or `nil` when it cannot be named — which
    /// includes the ordinary case of the user not having permitted the lookup.
    func placeName(for coordinate: AssetCoordinate) async -> String?
}

/// The resolver that never asks anyone anything.
///
/// The default, and the one every test gets: a viewer built without an explicit
/// resolver makes no network call, so a lookup can only happen where somebody
/// deliberately wired one in.
public struct NoPlaceNameResolver: PlaceNameResolver {
    public init() {}
    public func placeName(for _: AssetCoordinate) async -> String? { nil }
}

/// Resolves place names through CoreLocation, when the device is permitted to.
///
/// The permission check is *inside* the resolver rather than at the call site,
/// so a second caller cannot forget it. That is the difference between a policy
/// and a convention.
public struct SystemPlaceNameResolver: PlaceNameResolver {
    private let preference: PlaceNamePreference

    public init(preference: PlaceNamePreference = PlaceNamePreference()) {
        self.preference = preference
    }

    public func placeName(for coordinate: AssetCoordinate) async -> String? {
        guard preference.isEnabled else { return nil }
        let location = CLLocation(latitude: coordinate.latitude, longitude: coordinate.longitude)
        guard let placemark = try? await CLGeocoder().reverseGeocodeLocation(location).first
        else { return nil }
        // Locality first, then the wider names — a photo taken outside any town
        // still has a region worth naming, and "nil" would drop the row entirely.
        return placemark.locality
            ?? placemark.subAdministrativeArea
            ?? placemark.administrativeArea
            ?? placemark.country
    }
}
