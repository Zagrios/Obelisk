use super::*;
use std::{cell::RefCell, convert::Infallible};

const ARM64: u32 = 0x0100_000c;
const X86_64: u32 = 0x0100_0007;
const I386: u32 = 7;

fn thin(cpu: u32) -> Vec<u8> {
    let mut bytes = vec![0xcf, 0xfa, 0xed, 0xfe];
    bytes.extend(cpu.to_le_bytes());
    bytes.resize(64, 0);
    bytes
}

fn thin_32(cpu: u32) -> Vec<u8> {
    let mut bytes = vec![0xce, 0xfa, 0xed, 0xfe];
    bytes.extend(cpu.to_le_bytes());
    bytes.resize(64, 0);
    bytes
}

fn universal(cpus: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0xca, 0xfe, 0xba, 0xbe];
    bytes.extend((cpus.len() as u32).to_be_bytes());
    for cpu in cpus {
        bytes.extend(cpu.to_be_bytes());
        bytes.extend([0; 16]);
    }
    bytes
}

/// The plist `plutil -convert binary1` writes for CFBundleName "Café",
/// CFBundleExecutable "My Game" and LSMinimumSystemVersion "11.0".
const BINARY_PLIST: &[u8] = &[
    0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xd3, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x5f,
    0x10, 0x12, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x45, 0x78, 0x65, 0x63, 0x75, 0x74,
    0x61, 0x62, 0x6c, 0x65, 0x5f, 0x10, 0x16, 0x4c, 0x53, 0x4d, 0x69, 0x6e, 0x69, 0x6d, 0x75, 0x6d,
    0x53, 0x79, 0x73, 0x74, 0x65, 0x6d, 0x56, 0x65, 0x72, 0x73, 0x69, 0x6f, 0x6e, 0x5c, 0x43, 0x46,
    0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x4e, 0x61, 0x6d, 0x65, 0x57, 0x4d, 0x79, 0x20, 0x47, 0x61,
    0x6d, 0x65, 0x54, 0x31, 0x31, 0x2e, 0x30, 0x64, 0x00, 0x43, 0x00, 0x61, 0x00, 0x66, 0x00, 0xe9,
    0x08, 0x0f, 0x24, 0x3d, 0x4a, 0x52, 0x57, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x60,
];

fn xml_plist(executable: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>CFBundleName</key>\n\t<string>Game</string>\n\
         \t<key>CFBundleExecutable</key>\n\t<string>{executable}</string>\n</dict>\n</plist>\n"
    )
    .into_bytes()
}

const ARM: Slices = Slices(1);
const INTEL: Slices = Slices(2);

#[test]
fn the_arm64_and_64_bit_intel_slices_are_found_in_thin_and_universal_programs() {
    assert_eq!(slices(&thin(ARM64)), ARM);
    assert_eq!(slices(&thin(X86_64)), INTEL);
    assert_eq!(slices(&universal(&[X86_64, ARM64])), Slices(3));
    assert_eq!(slices(&universal(&[I386, ARM64])), ARM);
    // A 32-bit program holds no slice a launch runs, nor a 32-bit header
    // of another architecture.
    assert!(slices(&thin_32(I386)).is_empty());
    assert!(slices(&thin_32(ARM64)).is_empty());
    assert!(slices(&thin_32(X86_64)).is_empty());
    assert!(slices(&universal(&[I386])).is_empty());
    // A Java class file: 0xcafebabe, then its version.
    assert!(slices(&[0xca, 0xfe, 0xba, 0xbe, 0x00, 0x00, 0x00, 0x34]).is_empty());
    assert!(slices(b"#!/bin/sh\n").is_empty());
    assert!(slices(&[]).is_empty());
    // A slice table cut short names no slice beyond its end.
    assert_eq!(slices(&universal(&[X86_64, ARM64])[..30]), INTEL);
    assert!(slices(&universal(&[X86_64, ARM64])[..10]).is_empty());
}

#[test]
fn a_universal_binary_of_64_bit_entries_or_either_byte_order_is_read() {
    let mut wide = vec![0xca, 0xfe, 0xba, 0xbf, 0, 0, 0, 2];
    for cpu in [X86_64, ARM64] {
        wide.extend(cpu.to_be_bytes());
        wide.extend([0; 28]);
    }
    assert_eq!(slices(&wide), Slices(3));
    let mut swapped = vec![0xbe, 0xba, 0xfe, 0xca];
    swapped.extend(1u32.to_le_bytes());
    swapped.extend(ARM64.to_le_bytes());
    swapped.extend([0; 16]);
    assert_eq!(slices(&swapped), ARM);
}

#[test]
fn the_bundle_executable_is_read_from_xml_and_binary_plists() {
    assert_eq!(
        bundle_executable(&xml_plist("Tom &amp; Jerry&#x21;")).as_deref(),
        Some("Tom & Jerry!")
    );
    assert_eq!(bundle_executable(BINARY_PLIST).as_deref(), Some("My Game"));
    for unsafe_name in ["", "..", "MacOS/Game", "Bad&unknown;"] {
        assert_eq!(bundle_executable(&xml_plist(unsafe_name)), None);
    }
    assert_eq!(bundle_executable(b"<plist><dict></dict></plist>"), None);
    assert_eq!(bundle_executable(&BINARY_PLIST[..100]), None);
}

/// A build listing whose files read from memory, recording what was read.
struct Build {
    listing: Listing<usize>,
    files: Vec<(String, Vec<u8>)>,
    reads: RefCell<Vec<(String, u64)>>,
}

impl Build {
    fn new(files: &[(&str, Vec<u8>)]) -> Self {
        let mut listing = Listing::default();
        for (index, (path, bytes)) in files.iter().enumerate() {
            listing.file(path, index, bytes.len() as u64);
        }
        Self {
            listing,
            files: files
                .iter()
                .map(|(path, bytes)| ((*path).to_owned(), bytes.clone()))
                .collect(),
            reads: RefCell::default(),
        }
    }

    async fn runs(&self, executable: &str, libraries: &[&str]) -> bool {
        let read = |file: usize, bytes: u64| {
            let (path, content) = &self.files[file];
            self.reads.borrow_mut().push((path.clone(), bytes));
            std::future::ready(Ok::<_, Infallible>(content[..bytes as usize].to_vec()))
        };
        runs(&self.listing, executable, libraries, read)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_bundle_runs_when_its_program_and_steam_library_hold_a_slice_in_common() {
    let mut program = universal(&[X86_64, ARM64]);
    program.resize(10_000, 0);
    let build = Build::new(&[
        ("Game.app/Contents/Info.plist", xml_plist("Game")),
        ("Game.app/Contents/MacOS/Game", program),
        (
            "Game.app/Contents/Frameworks/libsteam_api.dylib",
            universal(&[X86_64, ARM64]),
        ),
    ]);
    assert!(build.runs("game.APP", &["libsteam_api.dylib"]).await);
    // Only a program's head is read.
    assert!(
        build
            .reads
            .borrow()
            .contains(&("Game.app/Contents/MacOS/Game".into(), PROGRAM_HEAD))
    );
    assert!(build.runs("Game.app", &[]).await);
}

#[tokio::test]
async fn a_32_bit_program_or_a_library_of_no_shared_architecture_does_not_run() {
    // Hotline Miami's Mac build names its i386 program itself.
    let build = Build::new(&[
        ("Hotline Miami.app/Contents/MacOS/YoYo70", thin_32(I386)),
        (
            "Hotline Miami.app/Contents/MacOS/libsteam_api.dylib",
            universal(&[I386]),
        ),
    ]);
    assert!(
        !build
            .runs(
                "Hotline Miami.app/Contents/MacOS/YoYo70",
                &["libsteam_api.dylib"]
            )
            .await
    );
    // The program holds both, but its library only Intel: the launch runs
    // them as Intel, under Rosetta.
    let build = Build::new(&[
        ("Game.app/Contents/Info.plist", xml_plist("Game")),
        ("Game.app/Contents/MacOS/Game", universal(&[X86_64, ARM64])),
        ("Game.app/Contents/MacOS/libsteam_api.dylib", thin(X86_64)),
    ]);
    assert!(build.runs("Game.app", &["libsteam_api.dylib"]).await);
    // An arm64 program with an Intel library, or the reverse, shares nothing.
    let build = Build::new(&[
        ("Game.app/Contents/Info.plist", xml_plist("Game")),
        ("Game.app/Contents/MacOS/Game", thin(ARM64)),
        ("Game.app/Contents/MacOS/libsteam_api.dylib", thin(X86_64)),
    ]);
    assert!(!build.runs("Game.app", &["libsteam_api.dylib"]).await);
    assert!(build.runs("Game.app", &[]).await);
}

#[tokio::test]
async fn an_intel_program_runs_under_rosetta_with_an_intel_steam_library() {
    let build = Build::new(&[
        ("Game.app/Contents/Info.plist", xml_plist("Game")),
        ("Game.app/Contents/MacOS/Game", thin(X86_64)),
        (
            "Game.app/Contents/MacOS/libsteam_api.dylib",
            universal(&[I386, X86_64]),
        ),
    ]);
    assert!(build.runs("Game.app", &["libsteam_api.dylib"]).await);
    assert!(build.runs("Game.app", &[]).await);
}

#[tokio::test]
async fn a_missing_program_plist_or_required_library_does_not_run() {
    let build = Build::new(&[
        ("Game.app/Contents/Info.plist", xml_plist("Other")),
        ("Game.app/Contents/MacOS/Game", thin(ARM64)),
        (
            "run.sh",
            b"#!/bin/sh\nexec Game.app/Contents/MacOS/Game\n".to_vec(),
        ),
    ]);
    assert!(!build.runs("Game.app", &[]).await);
    assert!(!build.runs("Missing.app", &[]).await);
    assert!(!build.runs("run.sh", &[]).await);
    assert!(
        !build
            .runs("Game.app/Contents/MacOS/Game", &["libsteam_api.dylib"])
            .await
    );
    assert!(build.runs("Game.app/Contents/MacOS/Game", &[]).await);
}

#[tokio::test]
async fn links_lead_to_their_targets_and_a_cycle_to_nothing() {
    let mut build = Build::new(&[
        ("Game.app/Contents/Info.plist", xml_plist("Game")),
        ("Game.app/Contents/MacOS/Game-arm64", thin(ARM64)),
        (
            "Game.app/Contents/Frameworks/Steam.framework/Versions/A/libsteam_api.dylib",
            thin(ARM64),
        ),
    ]);
    build.listing.link(
        "Game.app/Contents/MacOS/Game",
        "Game.app/Contents/MacOS/Game-arm64",
    );
    build.listing.link(
        "Game.app/Contents/Frameworks/Steam.framework/Versions/Current",
        "Game.app/Contents/Frameworks/Steam.framework/Versions/A",
    );
    assert!(build.runs("Game.app", &["libsteam_api.dylib"]).await);
    // A cycle never resolves.
    build.listing.link(
        "Loop.app/Contents/Info.plist",
        "Loop.app/Contents/Info.plist",
    );
    build.listing.file("Loop.app/Contents/MacOS/Loop", 0, 1);
    assert!(!build.runs("Loop.app", &[]).await);
}

#[tokio::test]
async fn the_library_a_launch_would_load_decides() {
    // A folder's own files come before its subfolders, and the last
    // subfolder is searched first.
    let build = Build::new(&[
        ("Game/Game", thin(ARM64)),
        ("Game/a/libsteam_api.dylib", thin(ARM64)),
        ("Game/b/libsteam_api.dylib", thin(X86_64)),
        ("Game/.hidden/libsteam_api.dylib", thin(ARM64)),
        ("Other/libsteam_api.dylib", thin(ARM64)),
    ]);
    assert!(!build.runs("Game/Game", &["libsteam_api.dylib"]).await);
    let build = Build::new(&[
        ("Game/Game", thin(ARM64)),
        ("Game/LIBSTEAM_API.dylib", thin(ARM64)),
        ("Game/b/libsteam_api.dylib", thin(X86_64)),
    ]);
    assert!(build.runs("Game/Game", &["libsteam_api.dylib"]).await);
}

#[test]
fn paths_take_either_separator_and_resolve_their_dots() {
    assert_eq!(
        normalized(r"Game.app\Contents/./MacOS//Game"),
        "Game.app/Contents/MacOS/Game"
    );
    assert_eq!(normalized("a/b/../c"), "a/c");
    assert_eq!(normalized("../../a"), "a");
}
