# macOS distribution

## Generating the DMG

From the repository root, on a Mac with Xcode and Rust installed:

```sh
./scripts/build-dmg.sh
```

The script builds the Release configuration, verifies the app signature, copies the bundle with `ditto`, and adds a link to `/Applications`. It creates a compressed, read-only HFS+ disk image with `hdiutil`, then verifies the image and calculates its SHA-256 checksum. The bundle statically links the Rust client. Installing the app requires neither Cargo nor Xcode.

Output files are saved in `build/dist/`:

```text
Forge-0.1.0-arm64.dmg
Forge-0.1.0-arm64.dmg.sha256
```

The version comes from the bundle and the architecture from the executable. The project currently builds for the host Mac rather than producing a universal binary. `arm64` denotes Apple Silicon Macs. The minimum target is macOS 14. This build has been verified on the development Mac, not on every supported macOS version.

To package an existing Release build or choose an output directory:

```sh
./scripts/build-dmg.sh --skip-build
./scripts/build-dmg.sh --output-dir /path/to/deliverables
```

The script replaces output files with the same names. Steam data, Keychain items, and downloaded games are never included in the disk image.

## Installing

1. Open the `.dmg` file.
2. Drag **Forge** to **Applications**.
3. Eject the disk image and open Forge from Applications.

The catalog and sessions remain associated with `com.zagrios.forge`, regardless of the app's location.

## Signing and publishing

The DMG preserves the Release signature. The current configuration uses the same Apple Development certificate for Debug and Release so the Keychain recognizes Forge after a rebuild. `Config/Signing.xcconfig` is shared by the app and its tests. The exact certificate and team can be set in `Config/Signing.local.xcconfig`, which Git ignores. The repository stores no certificates or private keys.

`scripts/verify-app-signature.sh` checks signature integrity and requires an Apple-issued certificate. It runs after builds and tests, and before creating a DMG, including with `--skip-build`. An ad hoc signature causes an error instead of producing an artifact that would ask for Keychain access again after each binary change.

After switching from an ad hoc build to this signature, users may need to grant access to the existing Steam session once more with "Always Allow". The token stays in the Keychain. Forge's code does not change its contents or access rules. This development signature does not make the file a notarized app or establish Gatekeeper approval on another Mac.

Public distribution still requires a **Developer ID Application** identity, Hardened Runtime, and Apple's notarization process, followed by stapling the ticket to the artifact. The Apple Development certificate available on the Mac does not replace Developer ID. The script sends no files to Apple and publishes nothing.

Reference: [Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

### Keychain access validation

On September 15, 2026, an isolated test created a dummy Keychain value with system interaction disabled. After rebuilding a different binary with the same identifier, the ad hoc signature caused a read failure, `errSecAuthFailed` (-25293). With the same Apple Development certificate on both binaries, both reads succeeded. The dummy entries were then deleted and their absence verified. This test changed neither a real Steam session nor any existing access rule.

The Forge Debug and Release apps also produced the same designated signing requirement despite their different binary hashes. Swift tests passed, and the verification script rejected the ad hoc control while accepting the certificate-signed control.

## Finder layout

`packaging/macos/FinderLayout.dsstore` defines a 640 × 380 window with Forge on the left and Applications on the right. This template contains no user paths. The packaging script copies it without AppleScript or a Python dependency.

The optional generator, `packaging/macos/generate-layout.py`, describes the positions and tool versions needed to change the template. After renaming the bundle, update `Forge.app` in both the generator and the packaging script. Regenerate the template only when its layout changes.
