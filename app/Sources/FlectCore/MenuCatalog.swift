import Foundation

/// The menu tree exported by the engine (`export-menu`), the single source of truth for IDs and labels.
public struct MenuEntry: Equatable {
    public let id: String
    public let labelKey: String
    public let label: String
    public let children: [MenuEntry]

    init?(_ v: JSONValue) {
        guard let id = v["id"]?.string else { return nil }
        self.id = id
        labelKey = v["label_key"]?.string ?? "menu.\(id)"
        label = v["label"]?.string ?? id
        children = (v["children"]?.array ?? []).compactMap(MenuEntry.init)
    }
}

public struct MenuCatalog: Equatable {
    public let entries: [MenuEntry]

    public init?(menu: JSONValue) {
        guard let list = menu["entries"]?.array else { return nil }
        entries = list.compactMap(MenuEntry.init)
    }

    public func find(_ id: String) -> MenuEntry? {
        func walk(_ es: [MenuEntry]) -> MenuEntry? {
            for e in es {
                if e.id == id { return e }
                if let f = walk(e.children) { return f }
            }
            return nil
        }
        return walk(entries)
    }
}
