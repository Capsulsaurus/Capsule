import Foundation

// MARK: - The section table

/// The one declaration of what sections exist, in what order, under which
/// heading, and where each surfaces on a phone.
///
/// Array order is display order for every shell, so the iPad sidebar, the
/// iPhone tab bar, and the iPhone overflow list cannot drift out of agreement.
/// Four sections are promoted to the phone's tab bar — Library, Browse, Search,
/// Settings. Albums gives up the tab it used to hold and becomes the first row
/// of Browse, because a tab bar that carries four of twenty sections has to
/// spend one of them on reaching the other sixteen.
public extension SidebarItem {
    /// Every section, in sidebar order.
    static let descriptors: [SidebarItemDescriptor] = [
        libraryDescriptor,
        SidebarItemDescriptor(
            item: .memories, rootRoute: .memories,
            titleKey: "app.sidebar.memories", systemImage: "sparkles.rectangle.stack",
            placement: .sidebar, group: .library
        ),
        SidebarItemDescriptor(
            item: .duplicates, rootRoute: .duplicates,
            titleKey: "app.sidebar.duplicates", systemImage: "square.on.square",
            placement: .sidebar, group: .library
        ),
        SidebarItemDescriptor(
            item: .trash, rootRoute: .trash,
            titleKey: "app.sidebar.trash", systemImage: "trash",
            placement: .sidebar, group: .library
        ),
        SidebarItemDescriptor(
            item: .hidden, rootRoute: .hidden,
            titleKey: "app.sidebar.hidden", systemImage: "eye.slash",
            placement: .sidebar, group: .library
        ),
        SidebarItemDescriptor(
            item: .browse, rootRoute: .browse,
            titleKey: "app.sidebar.browse", systemImage: "square.grid.2x2",
            placement: .phoneTab, group: .collections
        ),
        SidebarItemDescriptor(
            item: .albums, rootRoute: .albums,
            titleKey: "app.sidebar.albums", systemImage: "rectangle.stack",
            placement: .sidebar, group: .collections
        ),
        SidebarItemDescriptor(
            item: .people, rootRoute: .people,
            titleKey: "app.sidebar.people", systemImage: "person.2",
            placement: .sidebar, group: .collections
        ),
        SidebarItemDescriptor(
            item: .places, rootRoute: .places,
            titleKey: "app.sidebar.places", systemImage: "map",
            placement: .sidebar, group: .collections
        ),
        SidebarItemDescriptor(
            item: .search, rootRoute: .search(.all, text: nil),
            titleKey: "app.sidebar.search", systemImage: "magnifyingglass",
            placement: .tab, group: .collections
        ),
        SidebarItemDescriptor(
            item: .transfers, rootRoute: .transferCenter,
            titleKey: "app.sidebar.transfers", systemImage: "arrow.up.arrow.down.circle",
            placement: .sidebar, group: .activity
        ),
        SidebarItemDescriptor(
            item: .imports, rootRoute: .imports,
            titleKey: "app.sidebar.imports", systemImage: "square.and.arrow.down",
            placement: .sidebar, group: .activity
        ),
        SidebarItemDescriptor(
            item: .shares, rootRoute: .shares,
            titleKey: "app.sidebar.shares", systemImage: "link",
            placement: .sidebar, group: .activity
        ),
        SidebarItemDescriptor(
            item: .drops, rootRoute: .drops,
            titleKey: "app.sidebar.drops", systemImage: "tray.and.arrow.down",
            placement: .sidebar, group: .activity
        ),
        SidebarItemDescriptor(
            item: .quarantine, rootRoute: .quarantine,
            titleKey: "app.sidebar.quarantine", systemImage: "exclamationmark.shield",
            placement: .sidebar, group: .activity
        ),
        SidebarItemDescriptor(
            item: .devices, rootRoute: .devices,
            titleKey: "app.sidebar.devices", systemImage: "laptopcomputer.and.iphone",
            placement: .sidebar, group: .system
        ),
        SidebarItemDescriptor(
            item: .peers, rootRoute: .peers,
            titleKey: "app.sidebar.peers", systemImage: "network",
            placement: .sidebar, group: .system
        ),
        SidebarItemDescriptor(
            item: .federation, rootRoute: .federation,
            titleKey: "app.sidebar.federation", systemImage: "globe",
            placement: .sidebar, group: .system
        ),
        SidebarItemDescriptor(
            item: .storage, rootRoute: .storage,
            titleKey: "app.sidebar.storage", systemImage: "internaldrive",
            placement: .sidebar, group: .system
        ),
        SidebarItemDescriptor(
            item: .settings, rootRoute: .settings(.default),
            titleKey: "app.sidebar.settings", systemImage: "gearshape",
            placement: .tab, group: .system
        ),
    ]

    /// Library's row, named separately so ``SidebarItem/descriptor`` has a
    /// total, non-`nil` fallback without a force unwrap. `SidebarCatalogTests`
    /// asserts the fallback is unreachable by checking every case has a row.
    static let libraryDescriptor = SidebarItemDescriptor(
        item: .library, rootRoute: .timeline(.all),
        titleKey: "app.sidebar.library", systemImage: "photo.on.rectangle.angled",
        placement: .tab, group: .library
    )
}

// MARK: - Derived lookups

public extension SidebarItem {
    /// This section's row in the table.
    var descriptor: SidebarItemDescriptor { Self.descriptorsByItem[self] ?? Self.libraryDescriptor }

    /// The route a freshly-selected section shows.
    var rootRoute: Route { descriptor.rootRoute }

    /// The catalog key for this section's label.
    var titleKey: String { descriptor.titleKey }

    /// The SF Symbol for this section. Not user-facing text, so not a key.
    var systemImage: String { descriptor.systemImage }

    /// Where the compact shell surfaces this section.
    var placement: SidebarPlacement { descriptor.placement }

    /// The sidebar heading this section sits under.
    var group: SidebarGroup { descriptor.group }

    /// The sections the iPhone tab bar shows, in order.
    static let tabs: [SidebarItem] = descriptors
        .filter { $0.placement.isPhoneTab }
        .map(\.item)

    /// The rows the iPad and Mac sidebar lists, in order — every section except
    /// the one whose only job is to index the others.
    static let sidebarRows: [SidebarItem] = descriptors
        .filter { $0.placement.isSidebarRow }
        .map(\.item)

    /// What the Browse index offers: every section the phone's tab bar does not
    /// already carry.
    ///
    /// Derived rather than listed, so promoting a section to a tab removes it
    /// from Browse in the same edit that promotes it — the failure this whole
    /// table exists to prevent is a section that is in neither place.
    static let browsable: [SidebarItem] = descriptors
        .filter { $0.placement == .sidebar }
        .map(\.item)

    /// The tab that reaches this section on a phone: itself if it has one, and
    /// Browse otherwise.
    var compactHost: SidebarItem { placement.isPhoneTab ? self : .browse }

    /// Every section under one heading, in order.
    ///
    /// The *total* partition, which is what the catalog tests assert over. View
    /// code almost always wants one of the two filtered forms below: this one
    /// includes Browse, and a sidebar that renders it grows a row that opens an
    /// index of the rows beside it.
    static func sections(in group: SidebarGroup) -> [SidebarItem] {
        descriptors.filter { $0.group == group }.map(\.item)
    }

    /// One heading's sidebar rows, in order.
    static func sidebarRows(in group: SidebarGroup) -> [SidebarItem] {
        descriptors.filter { $0.group == group && $0.placement.isSidebarRow }.map(\.item)
    }

    /// One heading's Browse rows, in order.
    static func browsable(in group: SidebarGroup) -> [SidebarItem] {
        descriptors.filter { $0.group == group && $0.placement == .sidebar }.map(\.item)
    }

    private static let descriptorsByItem: [SidebarItem: SidebarItemDescriptor] =
        Dictionary(descriptors.map { ($0.item, $0) }, uniquingKeysWith: { first, _ in first })
}
