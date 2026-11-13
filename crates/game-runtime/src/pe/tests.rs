use super::*;

/// The machine of 32-bit x86 code.
pub(crate) const I386: u16 = 0x14c;

/// Where the fixtures' only section loads, and where their file stores it.
const LOADED: u32 = 0x1000;
const STORED: usize = 0x400;

/// A Windows program for the tests, as written in its file.
pub(crate) struct Fixture<'a> {
    pub machine: u16,
    pub dotnet: bool,
    pub imports: &'a [&'a str],
    pub delayed: &'a [&'a str],
    pub description: Option<&'a str>,
    /// Its version resource counts its texts' lengths in bytes, as some
    /// programs do, rather than in characters.
    pub lengths_in_bytes: bool,
}

impl Default for Fixture<'_> {
    fn default() -> Self {
        Self {
            machine: AMD64,
            dotnet: false,
            imports: &[],
            delayed: &[],
            description: None,
            lengths_in_bytes: false,
        }
    }
}

impl Fixture<'_> {
    /// An x86-64 program that imports `imports`.
    pub(crate) fn importing<'a>(imports: &'a [&'a str]) -> Fixture<'a> {
        Fixture {
            imports,
            ..Fixture::default()
        }
    }

    /// An x86-64 program whose version resource describes it as `description`.
    pub(crate) fn described(description: &str) -> Fixture<'_> {
        Fixture {
            description: Some(description),
            ..Fixture::default()
        }
    }

    /// The file.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();
        // Each directory's address; Obelisk reads no size.
        let mut directories = [0u32; 16];
        if !self.imports.is_empty() {
            directories[IMPORTS] = descriptors(&mut data, self.imports, 20, 12);
        }
        if !self.delayed.is_empty() {
            directories[DELAY_IMPORTS] = descriptors(&mut data, self.delayed, 32, 4);
        }
        if self.dotnet {
            directories[CLR] = put(&mut data, &[1; 72]);
        }
        if let Some(description) = self.description {
            directories[RESOURCES] = resources(&mut data, &self.version(description));
        }

        let pe32 = self.machine == I386;
        let optional_size = if pe32 { 224 } else { 240 };
        let mut file = vec![0u8; 64];
        file[..2].copy_from_slice(b"MZ");
        file[60..64].copy_from_slice(&64u32.to_le_bytes());
        file.extend_from_slice(b"PE\0\0");
        let mut coff = [0u8; 20];
        coff[..2].copy_from_slice(&self.machine.to_le_bytes());
        coff[2..4].copy_from_slice(&1u16.to_le_bytes());
        coff[16..18].copy_from_slice(&(optional_size as u16).to_le_bytes());
        file.extend_from_slice(&coff);
        let mut optional = vec![0u8; optional_size];
        let (count_at, first) = if pe32 { (92, 96) } else { (108, 112) };
        optional[..2].copy_from_slice(&(if pe32 { 0x10bu16 } else { 0x20b }).to_le_bytes());
        optional[count_at..count_at + 4].copy_from_slice(&16u32.to_le_bytes());
        for (index, address) in directories.into_iter().enumerate() {
            let at = first + index * 8;
            optional[at..at + 4].copy_from_slice(&address.to_le_bytes());
        }
        file.extend_from_slice(&optional);
        let mut section = [0u8; 40];
        section[..5].copy_from_slice(b".data");
        let size = (data.len() as u32).max(1);
        section[8..12].copy_from_slice(&size.to_le_bytes());
        section[12..16].copy_from_slice(&LOADED.to_le_bytes());
        section[16..20].copy_from_slice(&size.to_le_bytes());
        section[20..24].copy_from_slice(&(STORED as u32).to_le_bytes());
        file.extend_from_slice(&section);
        file.resize(STORED, 0);
        file.extend_from_slice(&data);
        file.resize(STORED + size as usize, 0);
        file
    }

    /// A version resource whose string table gives a company and
    /// `description`.
    fn version(&self, description: &str) -> Vec<u8> {
        let text = |key: &str, value: &str| {
            let units = value.encode_utf16().count() + 1;
            let length = if self.lengths_in_bytes {
                units * 2
            } else {
                units
            };
            block(key, 1, length, &utf16z(value), &[])
        };
        let mut strings = text("CompanyName", "A publisher");
        strings.extend(text("FileDescription", description));
        let table = block("040904b0", 1, 0, &[], &strings);
        let file_info = block("StringFileInfo", 1, 0, &[], &table);
        let mut fixed = [0u8; 52];
        fixed[..4].copy_from_slice(&0xfeef04bdu32.to_le_bytes());
        block("VS_VERSION_INFO", 0, 52, &fixed, &file_info)
    }
}

/// `bytes` at the next multiple of four in the section, and their address.
fn put(data: &mut Vec<u8>, bytes: &[u8]) -> u32 {
    data.resize(data.len().next_multiple_of(4), 0);
    let address = LOADED + data.len() as u32;
    data.extend_from_slice(bytes);
    address
}

/// An import directory of `dlls`: descriptors of `size` bytes, each naming
/// its DLL at `name_at`, then an empty one.
fn descriptors(data: &mut Vec<u8>, dlls: &[&str], size: usize, name_at: usize) -> u32 {
    let names: Vec<u32> = dlls
        .iter()
        .map(|dll| put(data, format!("{dll}\0").as_bytes()))
        .collect();
    let mut table = vec![0u8; size * (dlls.len() + 1)];
    for (index, name) in names.into_iter().enumerate() {
        let at = index * size + name_at;
        table[at..at + 4].copy_from_slice(&name.to_le_bytes());
    }
    put(data, &table)
}

/// A resource directory holding an icon type, then `version` as the version
/// resource of its first name and language.
fn resources(data: &mut Vec<u8>, version: &[u8]) -> u32 {
    let directory = |entries: &[(u32, u32)]| {
        let mut bytes = vec![0u8; 16];
        bytes[14..16].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        for (id, target) in entries {
            bytes.extend_from_slice(&id.to_le_bytes());
            bytes.extend_from_slice(&target.to_le_bytes());
        }
        bytes
    };
    const SUBDIRECTORY: u32 = 0x8000_0000;
    let root = data.len().next_multiple_of(4);
    let address = LOADED + root as u32;
    let mut tree = directory(&[(3, SUBDIRECTORY | 32), (VERSION, SUBDIRECTORY | 32)]);
    tree.extend(directory(&[(1, SUBDIRECTORY | 56)]));
    tree.extend(directory(&[(0x409, 80)]));
    let version_at = address + 96;
    tree.extend_from_slice(&version_at.to_le_bytes());
    tree.extend_from_slice(&(version.len() as u32).to_le_bytes());
    tree.extend_from_slice(&[0; 8]);
    tree.extend_from_slice(version);
    assert_eq!(put(data, &tree), address);
    address
}

/// A version resource block: its header, `key`, its value and `children`,
/// each part at a multiple of four, then padded for the next block.
fn block(key: &str, kind: u16, value_length: usize, value: &[u8], children: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0u8; 6];
    bytes[2..4].copy_from_slice(&(value_length as u16).to_le_bytes());
    bytes[4..6].copy_from_slice(&kind.to_le_bytes());
    bytes.extend(utf16z(key));
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    bytes.extend_from_slice(value);
    if !children.is_empty() {
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        bytes.extend_from_slice(children);
    }
    let length = bytes.len() as u16;
    bytes[..2].copy_from_slice(&length.to_le_bytes());
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    bytes
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// `fixture` written to a file of its own, read back.
fn opened(fixture: &Fixture<'_>) -> (tempfile::TempDir, Option<Program>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Program.exe");
    std::fs::write(&path, fixture.bytes()).unwrap();
    let program = Program::open(&path);
    (directory, program)
}

#[test]
fn a_program_tells_its_machine_and_whether_it_is_a_dotnet_program() {
    let (_directory, native) = opened(&Fixture::default());
    let native = native.unwrap();
    assert!(native.is_x86_64() && !native.is_dotnet());

    let (_directory, dotnet) = opened(&Fixture {
        dotnet: true,
        ..Fixture::default()
    });
    let dotnet = dotnet.unwrap();
    assert!(dotnet.is_x86_64() && dotnet.is_dotnet());

    let (_directory, x86) = opened(&Fixture {
        machine: I386,
        dotnet: true,
        ..Fixture::default()
    });
    let x86 = x86.unwrap();
    assert!(!x86.is_x86_64() && x86.is_dotnet());
}

#[test]
fn a_program_lists_the_dlls_it_imports_as_it_loads_and_as_it_first_calls_them() {
    let (_directory, program) = opened(&Fixture {
        imports: &["KERNEL32.dll", "dxgi.dll"],
        delayed: &["D3D12.dll"],
        ..Fixture::default()
    });
    assert_eq!(
        program.unwrap().imports(),
        ["kernel32.dll", "dxgi.dll", "d3d12.dll"]
    );
    let (_directory, x86) = opened(&Fixture {
        machine: I386,
        imports: &["d3d9.dll"],
        ..Fixture::default()
    });
    assert_eq!(x86.unwrap().imports(), ["d3d9.dll"]);
    let (_directory, none) = opened(&Fixture::default());
    assert!(none.unwrap().imports().is_empty());
}

#[test]
fn a_program_gives_the_description_of_its_version_resource() {
    let description = "Easy Anti-Cheat Bootstrapper (EOS)";
    let (_directory, program) = opened(&Fixture::described(description));
    assert_eq!(program.unwrap().description().as_deref(), Some(description));

    let (_directory, in_bytes) = opened(&Fixture {
        lengths_in_bytes: true,
        ..Fixture::described(description)
    });
    assert_eq!(
        in_bytes.unwrap().description().as_deref(),
        Some(description)
    );

    let (_directory, x86) = opened(&Fixture {
        machine: I386,
        ..Fixture::described("Launcher")
    });
    assert_eq!(x86.unwrap().description().as_deref(), Some("Launcher"));

    let (_directory, none) = opened(&Fixture::default());
    assert_eq!(none.unwrap().description(), None);
}

#[test]
fn a_file_that_is_no_program_or_stops_short_of_its_headers_reads_as_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Program.exe");
    for bytes in [&b""[..], b"MZ", b"#!/bin/sh\n", &[0u8; 512]] {
        std::fs::write(&path, bytes).unwrap();
        assert!(Program::open(&path).is_none());
    }
    assert!(Program::open(&directory.path().join("missing.exe")).is_none());

    let whole = Fixture {
        imports: &["dxgi.dll"],
        delayed: &["d3d12.dll"],
        dotnet: true,
        ..Fixture::described("A game")
    }
    .bytes();
    // Cut anywhere, the file reads as nothing or as what it still holds.
    for length in 0..whole.len() {
        std::fs::write(&path, &whole[..length]).unwrap();
        if let Some(program) = Program::open(&path) {
            assert!(program.imports().len() <= 2);
            let description = program.description();
            assert!(description.is_none() || description.as_deref() == Some("A game"));
        }
    }
}
