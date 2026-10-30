import AppKit
import Carbon.HIToolbox

/// The overlay's Shift-Tab, as Steam's own overlay takes it. Registering it
/// takes the key from every application until it is unregistered.
@MainActor
protocol OverlayHotKey: AnyObject {
    var isRegistered: Bool { get }
    /// Registers the key once; a second call keeps the first action.
    func register(_ action: @escaping @MainActor () -> Void)
    func unregister()
}

/// A Carbon hot key: macOS asks no permission for it, and the key it
/// consumes never reaches the game.
@MainActor
final class CarbonOverlayHotKey: OverlayHotKey {
    private let keyCode: UInt32
    private let modifiers: UInt32
    private static var nextIdentifier: UInt32 = 0
    private let identifier: EventHotKeyID

    init(keyCode: UInt32 = UInt32(kVK_Tab), modifiers: UInt32 = UInt32(shiftKey)) {
        self.keyCode = keyCode
        self.modifiers = modifiers
        Self.nextIdentifier += 1
        identifier = EventHotKeyID(signature: 0x4F42_4C4B, id: Self.nextIdentifier)
    }
    private var hotKey: EventHotKeyRef?
    private var handler: EventHandlerRef?
    private var action: (@MainActor () -> Void)?

    var isRegistered: Bool { hotKey != nil }

    func register(_ action: @escaping @MainActor () -> Void) {
        guard hotKey == nil else { return }
        self.action = action
        if handler == nil {
            var pressed = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
            InstallEventHandler(GetEventDispatcherTarget(), { _, event, context in
                guard let context else { return OSStatus(eventNotHandledErr) }
                let hotKey = Unmanaged<CarbonOverlayHotKey>.fromOpaque(context).takeUnretainedValue()
                // Carbon delivers hot keys on the main thread.
                return MainActor.assumeIsolated {
                    var pressed = EventHotKeyID()
                    guard GetEventParameter(event, EventParamName(kEventParamDirectObject),
                        EventParamType(typeEventHotKeyID), nil, MemoryLayout<EventHotKeyID>.size,
                        nil, &pressed) == noErr,
                        pressed.signature == hotKey.identifier.signature,
                        pressed.id == hotKey.identifier.id else { return OSStatus(eventNotHandledErr) }
                    hotKey.action?()
                    return noErr
                }
            }, 1, &pressed, Unmanaged.passUnretained(self).toOpaque(), &handler)
        }
        // "OBLK"
        RegisterEventHotKey(keyCode, modifiers, identifier, GetEventDispatcherTarget(), 0, &hotKey)
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
