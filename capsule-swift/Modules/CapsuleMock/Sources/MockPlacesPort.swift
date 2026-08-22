import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - PlacesPort

extension MockIntelligenceStore: PlacesPort {
    /// Clusters inside a region, at a zoom-appropriate granularity.
    ///
    /// Centroids are snapped to a grid whose cell size halves with each
    /// granularity step, and everything landing in one cell becomes one pin —
    /// which is what map clustering actually is, rather than a fixed list that
    /// pretends to zoom.
    ///
    /// Coordinates come back **in their stored datum** and are never converted.
    /// A GCJ-02 cluster on a WGS-84 map is marked approximate via
    /// ``GpsDatum/displaysAsApproximate``; the inverse conversion is lossy, and
    /// an unmarked pin is a pin in the wrong street.
    public func clusters(in region: MapRegion, granularity: Int) async throws -> [PlaceCluster] {
        let library = libraryStore.library
        var buckets: [String: PlaceBucket] = [:]
        for ordinal in MockTables.trips.indices {
            let place = MockTables.trips[ordinal]
            guard contains(region, latitude: place.latitude, longitude: place.longitude) else { continue }
            let key = cellKey(latitude: place.latitude, longitude: place.longitude, granularity: granularity)
            let count = tripAssetCount(library: library, ordinal: ordinal)
            var bucket = buckets[key] ?? PlaceBucket(centroid: centroid(of: place))
            bucket.trips.append(ordinal)
            bucket.assetTotal += count
            buckets[key] = bucket
        }
        if contains(region, latitude: MockTables.home.latitude, longitude: MockTables.home.longitude) {
            let key = cellKey(
                latitude: MockTables.home.latitude,
                longitude: MockTables.home.longitude,
                granularity: granularity
            )
            var bucket = buckets[key] ?? PlaceBucket(centroid: centroid(of: MockTables.home))
            bucket.assetTotal += homeAssetCount(library: library)
            buckets[key] = bucket
        }
        return buckets
            .filter { $0.value.assetTotal > 0 }
            .map { key, bucket in
                PlaceCluster(
                    id: key,
                    centroid: bucket.centroid,
                    assetCount: bucket.assetTotal,
                    keyAssetID: bucket.trips.first.flatMap { tripKeyAsset(library: library, ordinal: $0) }
                )
            }
            .sorted { $0.assetCount > $1.assetCount }
    }

    /// One map pin under construction: the trips that landed in its grid cell
    /// and their combined asset total.
    private struct PlaceBucket {
        var trips: [Int] = []
        var assetTotal = 0
        var centroid: Gps
    }

    /// The assets behind one pin.
    ///
    /// A trip occupies a contiguous run of days, and a day is a contiguous run
    /// of indices, so a pin's assets are a handful of index ranges rather than a
    /// predicate over the library — which is what makes this pageable at all.
    public func assets(in clusterID: String, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        let request = PageRequest(offset: offset, limit: limit)
        let library = libraryStore.library
        var identifiers: [AssetID] = []
        for ordinal in MockTables.trips.indices {
            let place = MockTables.trips[ordinal]
            guard cellKey(latitude: place.latitude, longitude: place.longitude, granularity: 6) == clusterID
                || clusterID.hasPrefix("cell:") && matchesAnyGranularity(place: place, clusterID: clusterID)
            else { continue }
            identifiers.append(contentsOf: tripIdentifiers(library: library, ordinal: ordinal))
        }
        let window = MockQueryEngine.window(identifiers, request: request)
        return try await Page(
            items: libraryStore.assets(for: window),
            request: request,
            totalCount: identifiers.count
        )
    }

    /// The region containing every located asset, for an initial camera
    /// position. `nil` when nothing is located.
    public func boundingRegion() async throws -> MapRegion? {
        guard libraryStore.library.assetCount > 0 else { return nil }
        let places = MockTables.trips + [MockTables.home]
        let latitudes = places.map(\.latitude)
        let longitudes = places.map(\.longitude)
        guard let minimumLatitude = latitudes.min(), let maximumLatitude = latitudes.max(),
              let minimumLongitude = longitudes.min(), let maximumLongitude = longitudes.max()
        else { return nil }
        return MapRegion(
            minimumLatitude: minimumLatitude - 1,
            maximumLatitude: maximumLatitude + 1,
            minimumLongitude: minimumLongitude - 1,
            maximumLongitude: maximumLongitude + 1
        )
    }

    // MARK: Geometry

    private func contains(_ region: MapRegion, latitude: Double, longitude: Double) -> Bool {
        latitude >= region.minimumLatitude && latitude <= region.maximumLatitude
            && longitude >= region.minimumLongitude && longitude <= region.maximumLongitude
    }

    private func cellKey(latitude: Double, longitude: Double, granularity: Int) -> String {
        let steps = min(12, max(1, granularity))
        let size = 180.0 / Double(1 << steps)
        return "cell:\(Int((latitude / size).rounded(.down))):\(Int((longitude / size).rounded(.down)))"
    }

    private func matchesAnyGranularity(place: MockTrip, clusterID: String) -> Bool {
        (1 ... 12).contains {
            cellKey(latitude: place.latitude, longitude: place.longitude, granularity: $0) == clusterID
        }
    }

    private func centroid(of place: MockTrip) -> Gps {
        Gps(latitude: place.latitude, longitude: place.longitude, source: .exif, datum: place.datum)
    }

    // MARK: Membership

    private func tripDayRange(library: MockLibrary, ordinal: Int) -> Range<Int> {
        let window = library.tripWindow(ordinal)
        return window.start ..< min(library.dayCount, window.start + window.length)
    }

    private func tripIdentifiers(library: MockLibrary, ordinal: Int) -> [AssetID] {
        tripDayRange(library: library, ordinal: ordinal).flatMap { dayIndex in
            library.indexRange(forDay: dayIndex)
                .filter { library.geolocation(for: MockAssetRef(kind: .live, index: $0)) != nil }
                .map { library.identifier(at: $0) }
        }
    }

    private func tripAssetCount(library: MockLibrary, ordinal: Int) -> Int {
        tripIdentifiers(library: library, ordinal: ordinal).count
    }

    private func tripKeyAsset(library: MockLibrary, ordinal: Int) -> AssetID? {
        tripIdentifiers(library: library, ordinal: ordinal).first
    }

    /// The home cluster's size.
    ///
    /// Everything not on a trip, scaled by the rate at which assets carry a fix
    /// at all. An **estimate** above the counting ceiling, and deliberately so:
    /// the exact answer is a full-library scan, and a map pin's badge is not
    /// worth 250 000 derivations on every pan.
    private func homeAssetCount(library: MockLibrary) -> Int {
        let tripTotal = MockTables.trips.indices.reduce(0) { $0 + tripAssetCount(library: library, ordinal: $1) }
        let located = Int(Double(library.assetCount) * 0.7)
        return max(0, located - tripTotal)
    }
}
