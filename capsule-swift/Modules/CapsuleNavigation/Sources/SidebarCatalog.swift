import Foundation

// MARK: - The section table

/// The one declaration of what sections exist, in what order, under which
/// heading, and where each surfaces on a phone.
///
/// Array order is display order for every shell, so the iPad sidebar, the
/// iPhone tab bar, and the iPhone overflow list cannot drift out of agreement.
/// The four promoted tabs are the four the app already ships — Library,
/// Collections (Albums), Search, Settings — so existing muscle memory survives
/// the arrival of the other fifteen sections.
public extension SidebarItem {
    /// Every section, in sidebar order.
    static let descriptors: [SidebarItemDescriptor] = [
        libraryDescriptor,
        SidebarItemDescriptor(
            item: .memories, rootRoute: .memories,
            titleKey: "ios.sidebar.memories", systemImage: "sparkles.rectangle.stack",
            placement: .overflow, group: .library
        ),
        SidebarItemDescriptor(
            item: .duplicates, rootRoute: .duplicates,
            titleKey: "ios.sidebar.duplicates", systemImage: "square.on.square",
            placement: .overflow, group: .library
        ),
        SidebarItemDescriptor(
            item: .trash, rootRoute: .trash,
            titleKey: "ios.sidebar.trash", systemImage: "trash",
            placement: .overflow, group: .library
        ),
        SidebarItemDescriptor(
            item: .hidden, rootRoute: .hidden,
            titleKey: "ios.sidebar.hidden", systemImage: "eye.slash",
            placement: .overflow, group: .library
        ),
        SidebarItemDescriptor(
            item: .albums, rootRoute: .albums,
            titleKey: "ios.sidebar.albums", systemImage: "rectangle.stack",
            placement: .tab, group: .collections
        ),
        SidebarItemDescriptor(
            item: .people, rootRoute: .people,
            titleKey: "ios.sidebar.people", systemImage: "person.2",
            placement: .overflow, group: .collections
        ),
        SidebarItemDescriptor(
            item: .places, rootRoute: .places,
            titleKey: "ios.sidebar.places", systemImage: "map",
            placement: .overflow, group: .collections
        ),
        SidebarItemDescriptor(
            item: .search, rootRoute: .search(.all, text: nil),
            titleKey: "ios.sidebar.search", systemImage: "magnifyingglass",
            placement: .tab, group: .collections
        ),
        SidebarItemDescriptor(
            item: .transfers, rootRoute: .transferCenter,
            titleKey: "ios.sidebar.transfers", systemImage: "arrow.up.arrow.down.circle",
            placement: .overflow, group: .activity
        ),
        SidebarItemDescriptor(
            item: .imports, rootRoute: .imports,
            titleKey: "ios.sidebar.imports", systemImage: "square.and.arrow.down",
            placement: .overflow, group: .activity
        ),
        SidebarItemDescriptor(
            item: .shares, rootRoute: .shares,
            titleKey: "ios.sidebar.shares", systemImage: "link",
            placement: .overflow, group: .activity
        ),
        SidebarItemDescriptor(
            item: .drops, rootRoute: .drops,
            titleKey: "ios.sidebar.drops", systemImage: "tray.and.arrow.down",
            placement: .overflow, group: .activity
        ),
        SidebarItemDescriptor(
            item: .quarantine, rootRoute: .quarantine,
            titleKey: "ios.sidebar.quarantine", systemImage: "exclamationmark.shield",
            placement: .overflow, group: .activity
        ),
        SidebarItemDescriptor(
            item: .devices, rootRoute: .devices,
            titleKey: "ios.sidebar.devices", systemImage: "laptopcomputer.and.iphone",
            placement: .overflow, group: .system
        ),
        SidebarItemDescriptor(
            item: .peers, rootRoute: .peers,
            titleKey: "ios.sidebar.peers", systemImage: "network",
            placement: .overflow, group: .system
        ),
        SidebarItemDescriptor(
            item: .federation, rootRoute: .federation,
            titleKey: "ios.sidebar.federation", systemImage: "globe",
            placement: .overflow, group: .system
        ),
        SidebarItemDescriptor(
            item: .storage, rootRoute: .storage,
            titleKey: "ios.sidebar.storage", systemImage: "internaldrive",
            placement: .overflow, group: .system
        ),
        SidebarItemDescriptor(
            item: .settings, rootRoute: .settings(.default),
            titleKey: "ios.sidebar.settings", systemImage: "gearshape",
            placement: .tab, group: .system
        ),
    ]

    /// Library's row, named separately so ``SidebarItem/descriptor`` has a
    /// total, non-`nil` fallback without a force unwrap. `SidebarCatalogTests`
    /// asserts the fallback is unreachable by checking every case has a row.
    static let libraryDescriptor = SidebarItemDescriptor(
        item: .library, rootRoute: .timeline(.all),
        titleKey: "ios.sidebar.library", systemImage: "photo.on.rectangle.angled",
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
        .filter { $0.placement == .tab }
        .map(\.item)

    /// The sections the iPhone reaches through its overflow surface, in order.
    static let overflow: [SidebarItem] = descriptors
        .filter { $0.placement == .overflow }
        .map(\.item)

    /// The sections under one sidebar heading, in order.
    static func sections(in group: SidebarGroup) -> [SidebarItem] {
        descriptors.filter { $0.group == group }.map(\.item)
    }

    private static let descriptorsByItem: [SidebarItem: SidebarItemDescriptor] =
        Dictionary(descriptors.map { ($0.item, $0) }, uniquingKeysWith: { first, _ in first })
}
