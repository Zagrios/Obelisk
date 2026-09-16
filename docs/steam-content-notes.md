# SteamPipe downloads

The `steam-kit::content` module receives authorization obtained through Steam: a depot ID, manifest, request code, depot key, and CDN server. It does not attempt to bypass licenses or DRM.

## Sources reviewed

These sources were reviewed on September 15, 2026, with pinned versions to make comparisons reproducible:

- [SteamKit: DepotManifest.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Types/DepotManifest.cs) covers manifest sections and filename encryption.
- [SteamKit: ContentManifest.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Base/Generated/ContentManifest.cs) defines protobuf field numbers and types.
- [SteamKit: CDN/Client.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Steam/CDN/Client.cs) covers HTTP paths, CDN tokens, and response sizes.
- [SteamKit: DepotChunk.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Steam/CDN/DepotChunk.cs), [VZipUtil.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Util/VZipUtil.cs), and [VZstdUtil.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Util/VZstdUtil.cs) cover compression envelopes and integrity checks.
- [DepotDownloader: ContentDownloader.cs](https://github.com/SteamRE/DepotDownloader/blob/e7474cb9ee8a87c0d917489b84c75667a1364aa1/DepotDownloader/ContentDownloader.cs) covers file assembly, executable permissions, and resuming.

SteamKit uses LGPL-2.1-or-later, and DepotDownloader uses GPL-2.0. Forge neither bundles nor runs their C# code. The Rust implementation describes the network formats observed in these references with its own interfaces, validation, and installation logic. There is no SteamCMD or .NET dependency.

## Implemented flow

1. Retrieve `depot/{depot}/manifest/{manifest}/5/{request_code}` over HTTPS. The last segment is omitted when the code is zero.
2. Read the single-file ZIP, then the manifest's protobuf sections. Verify the CRC when present and the requested identifiers.
3. Decrypt filenames when needed, using base64, AES-256 ECB for the IV, and AES-256 CBC PKCS7 for the null-terminated UTF-8 name.
4. Prevalidate all paths, sizes, and chunk ranges.
5. For each file, validate the existing file's SHA-1 or resume partial chunks whose SHA-1 and Adler-32 match the manifest.
6. Retrieve `depot/{depot}/chunk/{sha1}` over HTTPS. Decrypt AES and decode ZIP/DEFLATE, VZip/LZMA, or VZstd/Zstandard. Verify the container CRC, size, SHA-1, and Adler-32 with a zero seed.
7. Assemble the file in `.forge-content/{depot}/{manifest}/`. A file built only from freshly downloaded chunks is SHA-1 hashed as it is written; a file that reused chunks already on disk is read back in full so the check covers what actually landed there. Sync the file to disk, atomically replace the destination file, then sync the parent directory once per run of files sharing it. The previous file remains available if an error or cancellation occurs before replacement.

Within a depot, up to six chunks are requested from the CDN concurrently and delivered in manifest order to a dedicated disk thread that writes and hashes them, so no async runtime worker is blocked by disk I/O or decryption. The callback reports progress for each verified chunk. Cancellation wakes pending requests immediately, and partial data is retained for resuming. A file lock protects a destination against two simultaneous Forge installations. The coordinator also holds `.forge-install.lock` while installing all depots and writing the final receipt, preventing accounts or processes from interleaving their writes. The caller remains responsible for selecting or renewing the CDN.

## Module dependencies

| Library | Purpose |
| --- | --- |
| `reqwest`, `tokio` | Asynchronous HTTPS, timeouts, and cancellation |
| `prost` | Decoding manifest protobuf messages |
| `base64` | Encrypted filenames |
| `aes`, `cbc` | Steam encryption primitives and mode |
| `sha1`, `crc32fast` | File, chunk, and container integrity |
| `flate2` with `rust_backend` | DEFLATE for Steam ZIP files, without native zlib |
| `lzma-rs` | VZip LZMA in Rust |
| `ruzstd` | Zstandard in Rust, without native libzstd |
| `thiserror` | Typed errors |

Steam's simple ZIP container is parsed locally. No general-purpose ZIP extractor writes paths supplied by an archive. `tempfile`, already used by Forge, is also used in tests.

## Explicit limitations

- Protobuf v5 manifests are supported. The legacy binary v4 format is not.
- Manifests containing symbolic links are rejected before any game files are written. This may affect some macOS depots.
- Absolute paths, `..` traversal, existing links beneath the destination, special files, and files with multiple hard links are rejected. Case conflicts and files used as directories are also rejected. This validation does not isolate installation from another process owned by the same user replacing directories during installation.
- The signature section must be present, but this module does not authenticate its RSA signature. HTTPS, CRC, and SHA-1 are checked. The Steam protocol requires SHA-1.
- Manifest responses are capped at 128 MiB, and chunks and decompression dictionaries at 64 MiB. ZIP64, multivolume, and multi-entry variants are rejected.
- Files absent from a new manifest are not deleted. The downloader never runs install scripts or executables.
- This module does not yet provide VZipDelta differential updates, LAN transfers, or disk space reservation.

## Verification

`cargo test -p steam-kit content:: --lib` exercises all three chunk formats, encrypted filenames, incorrect hashes and sizes, truncated manifests, unsafe paths, existing links, a full download through a local test HTTP CDN, cancellation that preserves the installed file, and resuming without downloading valid chunks again. The test HTTP endpoint does not exist in normal builds, which construct only HTTPS URLs.

A second test compares the decoder against real public data in a SteamKit checkout without copying it into Forge: the encrypted manifest for depot 440 and one chunk of each format, ZIP, VZip, and VZstd. It passed on September 15, 2026. It helped reproduce and fix the line breaks Valve adds to some base64 filenames.

```sh
STEAMKIT_REFERENCE_DIRECTORY=/path/to/SteamKit \
  cargo test -p steam-kit decodes_upstream_reference -- --ignored
```

The opt-in `cm::tests::live_anonymous_cm_public_content_metadata` test also passed against Steam on September 15, 2026. It used an anonymous CM connection, obtained authorization for public depot 1004 of Steamworks SDK Redist, retrieved an HTTPS manifest, and downloaded a 340,120-byte file in a single chunk before decrypting, decompressing, and verifying it with SHA-1. The test keeps the file in memory and does not install the SDK.

These checks do not prove that a purchased game can be installed from a real Steam account. That verification requires the user to sign in voluntarily.
