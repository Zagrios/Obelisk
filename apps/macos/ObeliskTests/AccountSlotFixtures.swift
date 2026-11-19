@testable import Obelisk

/// Each storefront's original slot, where a test's account signs in unless
/// the test adds another.
extension AccountSlot {
    static let steam = AccountSlot(.steam)
    static let epic = AccountSlot(.epic)
    static let gog = AccountSlot(.gog)
}

/// The tab of each storefront's original slot.
extension LibraryTab {
    static let steam = LibraryTab.account(.steam)
    static let epic = LibraryTab.account(.epic)
    static let gog = LibraryTab.account(.gog)
}
