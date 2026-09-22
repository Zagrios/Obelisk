# Synthetic Legendary interoperability fixture

Generated on 2026-09-22 with the unchanged `Manifest`, `ManifestMeta`, `CDL`, `FML`, `FileManifest`, `CustomFields`, `ChunkInfo`, `ChunkPart` and `Chunk` classes from Legendary commit `42f6bdeadde3a9526dc8eb713763476999ac217b`. The public Python source was read and executed in a temporary virtual environment. No account credentials, game files or real saves were used. The repository stores only generated synthetic binary data, not Legendary source.

Sources: [manifest model](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/models/manifest.py), [chunk model](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/models/chunk.py), [rolling hash](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/utils/rolling_hash.py).

The compressed manifest uses feature level 18, metadata version 0, app name `gameaccount`, build version `2026.09.22-00.00.00`, and both folder custom fields. The single chunk uses GUID words `01020304 11121314 21222324 31323334` and contains the concatenation of `legendary-byte-proof` and bytes 0 through 255, padded to 1 MiB by `Chunk.data`. File entries are `Données/slot.sav`, `empty.sav`, and `settings.sav`; the empty file has no chunk parts. `Chunk.write()` and `Manifest.write()` used their default compression settings.

SHA-256:

- `save.manifest`: `605142f2eb741a00130cd54a30d5c67e98c4985ba5833e5b97d4790b84498714`
- `save.chunk`: `8c1a8cb8e3e0b25cb326b5a668ef263d586fdf3f80e422a1c836b4c61db45d06`

The reverse check also passed during implementation. Forge encoded three synthetic files, including an empty file and a Unicode path crossing a 1 MiB chunk boundary. Legendary restored all expected bytes from the resulting two chunks. The check compared file SHA-1, chunk SHA-1 and rolling hash, GUIDs, CRC32 group numbers, canonical chunk paths and both folder custom fields. This proves these fixture formats interoperate; it does not validate the live EGS service or a real game save.
