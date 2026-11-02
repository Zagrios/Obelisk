use super::*;

pub(crate) const ARM64: u32 = 0x0100_000c;
pub(crate) const INTEL: u32 = 0x0100_0007;

fn word(value: u32, little: bool) -> [u8; 4] {
    if little {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    }
}

fn long_word(value: u64, little: bool) -> [u8; 8] {
    if little {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    }
}

pub(crate) fn thin(cpu: u32, little: bool) -> Vec<u8> {
    let mut bytes = vec![0; 32];
    bytes[..4].copy_from_slice(&word(0xfeedfacf, little));
    bytes[4..8].copy_from_slice(&word(cpu, little));
    bytes
}

fn universal(cpus: &[u32], little: bool, wide: bool) -> Vec<u8> {
    let entry_size = if wide { 32 } else { 20 };
    let mut bytes = vec![0; 8 + cpus.len() * entry_size];
    let magic = if wide { 0xcafebabf } else { 0xcafebabe };
    bytes[..4].copy_from_slice(&word(magic, little));
    bytes[4..8].copy_from_slice(&word(cpus.len() as u32, little));
    for (index, cpu) in cpus.iter().enumerate() {
        let start = 8 + index * entry_size;
        let offset = bytes.len();
        bytes[start..start + 4].copy_from_slice(&word(*cpu, little));
        if wide {
            bytes[start + 8..start + 16].copy_from_slice(&long_word(offset as u64, little));
            bytes[start + 16..start + 24].copy_from_slice(&long_word(32, little));
        } else {
            bytes[start + 8..start + 12].copy_from_slice(&word(offset as u32, little));
            bytes[start + 12..start + 16].copy_from_slice(&word(32, little));
        }
        // The slice's byte order is independent of its enclosing fat header.
        bytes.extend(thin(*cpu, !little));
    }
    bytes
}

fn parse(bytes: &[u8]) -> Result<u8, Error> {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("executable");
    std::fs::write(&path, bytes).unwrap();
    slices(&path)
}

#[test]
fn selects_the_shared_arm64_slice_only() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let sdk = root.path().join("sdk");
    for (game_bytes, sdk_bytes, expected) in [
        (
            thin(ARM64, true),
            thin(ARM64, true),
            Some(Architecture::Arm64),
        ),
        // An Intel game never runs natively, whatever its library holds.
        (
            thin(INTEL, true),
            universal(&[INTEL, ARM64], false, false),
            None,
        ),
        (
            universal(&[INTEL, ARM64], false, false),
            universal(&[INTEL, ARM64], false, true),
            Some(Architecture::Arm64),
        ),
        (
            universal(&[INTEL, ARM64], true, true),
            thin(INTEL, true),
            None,
        ),
        (thin(ARM64, true), thin(INTEL, true), None),
        (thin(7, true), thin(7, true), None),
    ] {
        std::fs::write(&game, game_bytes).unwrap();
        std::fs::write(&sdk, sdk_bytes).unwrap();
        assert_eq!(Architecture::select(&game, &sdk).ok(), expected);
    }
}

#[test]
fn standalone_executable_runs_its_arm64_slice_without_an_sdk_dependency() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    for (bytes, expected) in [
        (thin(ARM64, true), Architecture::Arm64),
        (
            universal(&[INTEL, ARM64], false, false),
            Architecture::Arm64,
        ),
        (universal(&[INTEL, ARM64], true, true), Architecture::Arm64),
    ] {
        std::fs::write(&game, bytes).unwrap();
        assert_eq!(Architecture::preferred(&game).unwrap(), expected);
    }
    for bytes in [thin(INTEL, true), b"\xcf\xfa\xed\xfe".to_vec()] {
        std::fs::write(&game, bytes).unwrap();
        assert!(matches!(
            Architecture::preferred(&game),
            Err(Error::IncompatibleArchitecture)
        ));
    }
}

#[test]
fn reads_thin_and_both_fat_entry_widths_in_either_byte_order() {
    for little in [false, true] {
        assert_eq!(parse(&thin(ARM64, little)).unwrap(), 1);
        assert_eq!(parse(&thin(INTEL, little)).unwrap(), 2);
        for wide in [false, true] {
            assert_eq!(parse(&universal(&[ARM64], little, wide)).unwrap(), 1);
            assert_eq!(parse(&universal(&[INTEL], little, wide)).unwrap(), 2);
            assert_eq!(parse(&universal(&[INTEL, ARM64], little, wide)).unwrap(), 3);
        }
    }
}

#[test]
fn accepts_cpu_variants_with_different_subtypes() {
    let mut bytes = universal(&[INTEL, INTEL], false, false);
    // CPU_SUBTYPE_X86_64_ALL and CPU_SUBTYPE_X86_64_H share a CPU type.
    for (index, subtype) in [3_u32, 8].into_iter().enumerate() {
        let entry = 8 + index * 20;
        let slice = 48 + index * 32;
        bytes[entry + 4..entry + 8].copy_from_slice(&subtype.to_be_bytes());
        bytes[slice + 8..slice + 12].copy_from_slice(&subtype.to_le_bytes());
    }
    assert_eq!(parse(&bytes).unwrap(), 2);
}

#[test]
fn rejects_scripts_unsupported_cpu_and_truncated_headers() {
    let valid = universal(&[INTEL, ARM64], false, false);
    let mut thin32 = thin(INTEL, true);
    thin32[..4].copy_from_slice(&0xfeedface_u32.to_le_bytes());
    for bytes in [
        Vec::new(),
        b"#!/bin/sh\nexit 0\n".to_vec(),
        thin(7, true),
        thin32,
        thin(INTEL, true)[..31].to_vec(),
        valid[..20].to_vec(),
        valid[..valid.len() - 1].to_vec(),
    ] {
        assert!(matches!(
            parse(&bytes),
            Err(Error::IncompatibleArchitecture)
        ));
    }
    for count in [0_u32, 33] {
        let mut bytes = valid.clone();
        bytes[4..8].copy_from_slice(&count.to_be_bytes());
        assert!(matches!(
            parse(&bytes),
            Err(Error::IncompatibleArchitecture)
        ));
    }
}

#[test]
fn rejects_overlapping_table_and_slices_and_mislabeled_cpu() {
    for little in [false, true] {
        let valid = universal(&[INTEL, ARM64], little, false);
        let mut table_overlap = valid.clone();
        table_overlap[16..20].copy_from_slice(&word(8, little));
        let mut slice_overlap = valid.clone();
        slice_overlap[36..40].copy_from_slice(&word(48, little));
        let mut wrong_cpu = valid.clone();
        wrong_cpu[8..12].copy_from_slice(&word(ARM64, little));
        let mut small_slice = valid.clone();
        small_slice[20..24].copy_from_slice(&word(31, little));
        let mut outside_file = valid.clone();
        outside_file[16..20].copy_from_slice(&word(valid.len() as u32 + 1, little));
        for bytes in [
            table_overlap,
            slice_overlap,
            wrong_cpu,
            small_slice,
            outside_file,
        ] {
            assert!(matches!(
                parse(&bytes),
                Err(Error::IncompatibleArchitecture)
            ));
        }
    }
}

#[test]
fn rejects_fat64_offset_overflow_and_truncated_entry_in_both_byte_orders() {
    for little in [false, true] {
        let valid = universal(&[INTEL], little, true);
        let mut overflow = valid.clone();
        overflow[16..24].copy_from_slice(&long_word(u64::MAX - 15, little));
        let mut table_overlap = valid.clone();
        table_overlap[16..24].copy_from_slice(&long_word(8, little));
        for bytes in [overflow, table_overlap, valid[..39].to_vec()] {
            assert!(matches!(
                parse(&bytes),
                Err(Error::IncompatibleArchitecture)
            ));
        }
    }
}

#[cfg(unix)]
fn probe_script(root: &Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("probe");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn probe_success_and_failure_use_process_result_without_launchservices() {
    let root = tempfile::tempdir().unwrap();
    let path = probe_script(root.path(), "test \"$1\" = --probe");
    assert!(rosetta_available(&path).unwrap());
    probe_script(root.path(), "exit 7");
    assert!(matches!(
        rosetta_available(&path),
        Err(crate::supervisor::Error::RosettaUnavailable)
    ));
    assert!(matches!(
        rosetta_available(&root.path().join("missing")),
        Err(crate::supervisor::Error::Unavailable)
    ));
}

#[cfg(unix)]
#[test]
fn hung_probe_is_stopped_instead_of_being_treated_as_missing_rosetta() {
    let root = tempfile::tempdir().unwrap();
    let path = probe_script(root.path(), "exec /bin/sleep 30");
    let start = Instant::now();
    assert!(matches!(
        rosetta_available(&path),
        Err(crate::supervisor::Error::RosettaUnavailable)
    ));
    assert!(start.elapsed() < Duration::from_secs(10));
}
