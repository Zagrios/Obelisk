#import <AppKit/AppKit.h>
#include <string.h>

// This app exists only to request macOS's normal Intel-app launch path. It
// never starts a game, reads credentials, or changes Forge's launch state.
@interface ForgeRosettaDelegate : NSObject <NSApplicationDelegate>
@end

@implementation ForgeRosettaDelegate
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    (void)notification;
    // Finish AppKit's launch handshake before exiting, so LaunchServices can
    // deliver its launch completion. The real installer flow needs UI testing.
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC),
                   dispatch_get_main_queue(), ^{
        [NSApp terminate:nil];
    });
}
@end

int main(int argc, const char *argv[]) {
    // Successful execution of this x86_64-only binary on Apple silicon is the
    // readiness check. Direct execution cannot start our AppKit launch path.
    if (argc == 2 && strcmp(argv[1], "--probe") == 0) {
        return 0;
    }
    @autoreleasepool {
        NSApplication *application = [NSApplication sharedApplication];
        [application setActivationPolicy:NSApplicationActivationPolicyProhibited];
        __attribute__((objc_precise_lifetime))
        ForgeRosettaDelegate *delegate = [ForgeRosettaDelegate new];
        application.delegate = delegate;
        [application run];
    }
    return 0;
}
