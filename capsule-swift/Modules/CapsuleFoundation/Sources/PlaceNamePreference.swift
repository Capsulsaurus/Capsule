import Foundation

/// Whether this device may resolve a photo's coordinates into a place name.
///
/// **Off by default, and deliberately per device.**
///
/// Resolving "43.75, −79.42" into "Toronto" means sending the coordinates of a
/// photograph to Apple's geocoding service. Capsule's whole posture is that the
/// library works with no network and that nothing leaves the device unasked, so
/// this cannot be on by default and cannot be inferred from the fact that a
/// screen is showing a map — the map is drawn from tiles, which says nothing
/// about *this* photo; a reverse-geocode names it.
///
/// It is a **device** preference rather than a `LibrarySettings` field because
/// the thing being permitted is this device making a network call. Syncing it
/// would mean enabling geocoding on a phone by ticking a box on a laptop, which
/// is precisely the kind of ambient consent the posture exists to refuse.
///
/// Backed by `UserDefaults` for the same reason `HiddenStore` is: the value is
/// one boolean, it must survive a launch, and it must never travel.
///
/// `@unchecked Sendable` because `UserDefaults` carries no `Sendable`
/// conformance while being documented as thread-safe — every access below is one
/// atomic `bool(forKey:)` or `set(_:forKey:)` against a store that serialises
/// its own reads and writes. There is no state here to race on.
public struct PlaceNamePreference: @unchecked Sendable {
    /// The defaults key. Namespaced, so it cannot collide with a library value.
    public static let defaultsKey = "capsule.privacy.resolvePlaceNames"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Whether place names may be resolved. `false` until the user says
    /// otherwise — including on a fresh install, where `UserDefaults` answers
    /// `false` for an absent key, which is the answer we want.
    public var isEnabled: Bool {
        get { defaults.bool(forKey: Self.defaultsKey) }
        nonmutating set { defaults.set(newValue, forKey: Self.defaultsKey) }
    }
}
