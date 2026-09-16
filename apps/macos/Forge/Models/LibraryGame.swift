import Foundation

enum LocalGameKind: Sendable {
    case nativeMac, windowsExecutable
}

struct LibraryGame: Identifiable, Equatable, Sendable {
    let id: String
    let title: String
    let path: String
    let addedAt: Date
    let lastPlayedAt: Date?
    let kind: LocalGameKind

    var canLaunch: Bool { kind == .nativeMac }
}
