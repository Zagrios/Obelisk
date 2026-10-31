import AppKit
import Carbon.HIToolbox

/// EOS receives the same Shift-F3 as the player's keyboard. Posting is optional:
/// when macOS has not granted access the UI supplies the manual shortcut.
@MainActor
enum EpicSocialShortcut {
    static func post(_ application: OverlayApplication) -> Bool {
        guard CGPreflightPostEventAccess(),
              let running = NSRunningApplication(processIdentifier: application.pid),
              !running.isTerminated, running.isActive,
              let source = CGEventSource(stateID: .privateState) else { return false }
        let keys: [(CGKeyCode, Bool, CGEventFlags, CGEventType)] = [
            (CGKeyCode(kVK_Shift), true, .maskShift, .flagsChanged),
            (CGKeyCode(kVK_F3), true, .maskShift, .keyDown),
            (CGKeyCode(kVK_F3), false, .maskShift, .keyUp),
            (CGKeyCode(kVK_Shift), false, [], .flagsChanged)
        ]
        let events = keys.compactMap { key, down, flags, type -> CGEvent? in
            guard let event = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: down) else { return nil }
            event.flags = flags
            event.type = type
            return event
        }
        guard events.count == keys.count else { return false }
        for event in events { event.postToPid(application.pid) }
        return true
    }
}

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
    private var hotKey: EventHotKeyRef?
    private var handler: EventHandlerRef?
    private var action: (@MainActor () -> Void)?

    var isRegistered: Bool { hotKey != nil }

    func register(_ action: @escaping @MainActor () -> Void) {
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
        RegisterEventHotKey(UInt32(kVK_Tab), UInt32(shiftKey), identifier, GetEventDispatcherTarget(), 0, &hotKey)
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
