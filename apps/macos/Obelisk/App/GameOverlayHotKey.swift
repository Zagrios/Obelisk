import AppKit
import Carbon.HIToolbox

/// The keys that show the overlay: Shift-Tab, as Steam's own overlay takes
/// it, unless the player chose others in Settings, as for a game that uses
/// Shift-Tab itself.
struct OverlayShortcut: Equatable {
    /// The key's virtual code and Carbon's modifier bits, as a hot key takes them.
    let keyCode: UInt32
    let modifiers: UInt32
    /// How the key reads on its key cap, as its keyboard typed it when it
    /// was chosen. Tab reads in Obelisk's language instead.
    let key: String

    static let shiftTab = OverlayShortcut(keyCode: UInt32(kVK_Tab), modifiers: UInt32(shiftKey), key: "")
    private static var preference: String { "overlay.shortcut" }

    /// The keys a key press chooses, or nil for one that cannot be the
    /// overlay's: a key alone, which its game would lose, unless it is a
    /// function key.
    init?(_ event: NSEvent) {
        var modifiers: UInt32 = 0
        if event.modifierFlags.contains(.control) { modifiers |= UInt32(controlKey) }
        if event.modifierFlags.contains(.option) { modifiers |= UInt32(optionKey) }
        if event.modifierFlags.contains(.shift) { modifiers |= UInt32(shiftKey) }
        if event.modifierFlags.contains(.command) { modifiers |= UInt32(cmdKey) }
        let isTab = Int(event.keyCode) == kVK_Tab
        let named = Self.named[Int(event.keyCode)]
        let isFunctionKey = named?.hasPrefix("F") == true
        guard modifiers != 0 || isFunctionKey,
              let key = isTab ? "" : named ?? event.characters(byApplyingModifiers: [])?.uppercased(),
              isTab || !key.isEmpty
        else { return nil }
        self.init(keyCode: UInt32(event.keyCode), modifiers: modifiers, key: key)
    }

    init(keyCode: UInt32, modifiers: UInt32, key: String) {
        self.keyCode = keyCode
        self.modifiers = modifiers
        self.key = key
    }

    /// The shortcut `defaults` keeps, else Shift-Tab.
    init(defaults: UserDefaults?) {
        let kept = defaults?.dictionary(forKey: Self.preference)
        guard let keyCode = kept?["keyCode"] as? Int, let modifiers = kept?["modifiers"] as? Int,
              let key = kept?["key"] as? String
        else {
            self = .shiftTab
            return
        }
        self.init(keyCode: UInt32(keyCode), modifiers: UInt32(modifiers), key: key)
    }

    /// Keeps it in `defaults`; Shift-Tab is kept as no choice.
    func save(in defaults: UserDefaults?) {
        if self == .shiftTab {
            defaults?.removeObject(forKey: Self.preference)
        } else {
            defaults?.set(["keyCode": Int(keyCode), "modifiers": Int(modifiers), "key": key],
                          forKey: Self.preference)
        }
    }

    /// Its keys as key caps, modifiers first in the order macOS shows them.
    @MainActor var caps: [String] {
        var caps: [String] = []
        if modifiers & UInt32(controlKey) != 0 { caps.append("⌃") }
        if modifiers & UInt32(optionKey) != 0 { caps.append("⌥") }
        if modifiers & UInt32(shiftKey) != 0 { caps.append(L10n.string("ui.settings.key_shift")) }
        if modifiers & UInt32(cmdKey) != 0 { caps.append("⌘") }
        caps.append(keyCode == UInt32(kVK_Tab) ? L10n.string("ui.settings.key_tab") : key)
        return caps
    }

    /// Its keys in a sentence, as a menu spells a shortcut: ⌥⇥.
    var name: String {
        var name = ""
        if modifiers & UInt32(controlKey) != 0 { name += "⌃" }
        if modifiers & UInt32(optionKey) != 0 { name += "⌥" }
        if modifiers & UInt32(shiftKey) != 0 { name += "⇧" }
        if modifiers & UInt32(cmdKey) != 0 { name += "⌘" }
        return name + (keyCode == UInt32(kVK_Tab) ? "⇥" : key)
    }

    /// The keys that type no character of their own, as their key caps read.
    private static let named: [Int: String] = [
        kVK_Space: "␣", kVK_Return: "↩", kVK_Delete: "⌫", kVK_ForwardDelete: "⌦",
        kVK_LeftArrow: "←", kVK_RightArrow: "→", kVK_UpArrow: "↑", kVK_DownArrow: "↓",
        kVK_Home: "↖", kVK_End: "↘", kVK_PageUp: "⇞", kVK_PageDown: "⇟",
        kVK_F1: "F1", kVK_F2: "F2", kVK_F3: "F3", kVK_F4: "F4", kVK_F5: "F5", kVK_F6: "F6",
        kVK_F7: "F7", kVK_F8: "F8", kVK_F9: "F9", kVK_F10: "F10", kVK_F11: "F11", kVK_F12: "F12",
        kVK_F13: "F13", kVK_F14: "F14", kVK_F15: "F15", kVK_F16: "F16", kVK_F17: "F17", kVK_F18: "F18",
        kVK_F19: "F19", kVK_F20: "F20",
    ]
}

/// The overlay's hot key. Registering it takes the keys from every
/// application until it is unregistered.
@MainActor
protocol OverlayHotKey: AnyObject {
    var isRegistered: Bool { get }
    /// Registers `shortcut` once; a second call keeps the first keys and action.
    func register(_ shortcut: OverlayShortcut, _ action: @escaping @MainActor () -> Void)
    func unregister()
}

/// A Carbon hot key: macOS asks no permission for it, and the key it
/// consumes never reaches the game.
@MainActor
final class CarbonOverlayHotKey: OverlayHotKey {
    private var hotKey: EventHotKeyRef?
    private var handler: EventHandlerRef?
    private var action: (@MainActor () -> Void)?

    var isRegistered: Bool { hotKey != nil }

    func register(_ shortcut: OverlayShortcut, _ action: @escaping @MainActor () -> Void) {
        guard hotKey == nil else { return }
        self.action = action
        if handler == nil {
            var pressed = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
            InstallEventHandler(GetEventDispatcherTarget(), { _, _, context in
                guard let context else { return OSStatus(eventNotHandledErr) }
                let hotKey = Unmanaged<CarbonOverlayHotKey>.fromOpaque(context).takeUnretainedValue()
                // Carbon delivers hot keys on the main thread.
                MainActor.assumeIsolated { hotKey.action?() }
                return noErr
            }, 1, &pressed, Unmanaged.passUnretained(self).toOpaque(), &handler)
        }
        // "OBLK"
        let identifier = EventHotKeyID(signature: 0x4F42_4C4B, id: 1)
        RegisterEventHotKey(shortcut.keyCode, shortcut.modifiers, identifier, GetEventDispatcherTarget(), 0, &hotKey)
    }

    func unregister() {
        if let hotKey { UnregisterEventHotKey(hotKey) }
        hotKey = nil
        action = nil
    }

    isolated deinit {
        unregister()
        if let handler { RemoveEventHandler(handler) }
    }
}
