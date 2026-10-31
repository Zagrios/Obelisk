//! EOS's own social UI owns friend requests, parties and their game callbacks.
//! Fall Guys skips a 32-bit EOS bootstrapper on the Linux engine. Install the
//! standalone overlay into that engine's actual prefix before any game token
//! exists, rather than attempting to run the unsupported bootstrapper.
use super::Configuration;
use crate::{launch::Error, wine};
use obelisk_core::Platform;
use std::{fs, path::PathBuf};

pub(super) fn required(config: &Configuration) -> bool {
    config.platform == Platform::Windows
        && config.namespace == "50118b7f954e450f8823df1614b24e80"
        && config.app_name == "0a2d9f6403244d12969e11da6713137b"
}

pub(super) fn parent(wine: &wine::Config) -> PathBuf {
    wine.prefix.join("drive_c/obelisk")
}

/// Prefix preparation has waited for wineserver to exit, and the launch holds
/// the installation's reservation. No Wine process may overwrite user.reg
/// while this writes it. The C: path is valid in both engines; a host path is
/// not visible inside the Linux guest.
pub(super) fn register(wine: &wine::Config) -> Result<(), Error> {
    let path = wine.prefix.join("user.reg");
    if !fs::symlink_metadata(&path)?.file_type().is_file() {
        return Err(Error::Storage);
    }
    let bytes = obelisk_core::install::read_file_bounded(&path, 64 * 1024 * 1024)
        .map_err(|_| Error::Storage)?
        .ok_or(Error::Storage)?;
    let registry = String::from_utf8(bytes).map_err(|_| Error::Storage)?;
    if !registry.starts_with("WINE REGISTRY Version 2") {
        return Err(Error::Storage);
    }
    let updated = registry_with_overlay(&registry);
    if updated != registry {
        obelisk_core::write_atomic(&path, updated.as_bytes())?;
    }
    Ok(())
}

fn registry_with_overlay(registry: &str) -> String {
    const SECTION: &str = r"[Software\\Epic Games\\EOS]";
    const VALUE: &str = "\"OverlayPath\"=\"C:/obelisk/EOSOverlay\"\n";
    let mut result = String::with_capacity(registry.len() + 100);
    let mut in_section = false;
    let mut found = false;
    for line in registry.lines() {
        if line.starts_with('[') {
            in_section = line.split(']').next().is_some_and(|section| {
                section.eq_ignore_ascii_case(SECTION.strip_suffix(']').unwrap())
            });
            if in_section {
                found = true;
                result.push_str(line);
                result.push('\n');
                result.push_str(VALUE);
                continue;
            }
        }
        if in_section
            && line
                .split('=')
                .next()
                .is_some_and(|name| name.trim().eq_ignore_ascii_case("\"OverlayPath\""))
        {
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    if !found {
        result.push_str(&format!("\n{SECTION}\n{VALUE}"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_uses_the_active_engines_prefix_and_refuses_invalid_registry_files() {
        let (_directory, root) = crate::wine::tests::installation();
        let mut linux = crate::wine::tests::config(&root);
        linux.prefix = root.join("game-linux/prefix");
        linux.settings.engine = wine::Engine::Linux;
        let rosetta = crate::wine::tests::config(&root);
        for config in [&linux, &rosetta] {
            fs::create_dir_all(&config.prefix).unwrap();
            fs::write(config.prefix.join("user.reg"), "WINE REGISTRY Version 2\n").unwrap();
        }
        register(&linux).unwrap();
        assert!(
            fs::read_to_string(linux.prefix.join("user.reg"))
                .unwrap()
                .contains("C:/obelisk/EOSOverlay")
        );
        assert!(
            !fs::read_to_string(rosetta.prefix.join("user.reg"))
                .unwrap()
                .contains("OverlayPath")
        );
        assert!(parent(&linux).starts_with(&linux.prefix));
        fs::write(linux.prefix.join("user.reg"), "corrupt registry").unwrap();
        assert_eq!(register(&linux), Err(Error::Storage));
        assert_eq!(
            fs::read_to_string(linux.prefix.join("user.reg")).unwrap(),
            "corrupt registry"
        );
        fs::remove_file(linux.prefix.join("user.reg")).unwrap();
        std::os::unix::fs::symlink(
            rosetta.prefix.join("user.reg"),
            linux.prefix.join("user.reg"),
        )
        .unwrap();
        assert_eq!(register(&linux), Err(Error::Storage));
    }

    #[test]
    fn overlay_registration_preserves_other_keys_and_is_idempotent() {
        let registry = "WINE REGISTRY Version 2\n\n[Software\\\\Other]\n\"OverlayPath\"=\"leave me\"\n\n[Software\\\\Epic Games\\\\EOS] 123\n\"Other\"=dword:00000001\n\"OverlayPath\"=\"Z:/host/old\"\n\n[Software\\\\Next]\n\"Value\"=\"next\"\n";
        let updated = registry_with_overlay(registry);
        assert!(updated.contains("\"OverlayPath\"=\"leave me\""));
        assert!(updated.contains("\"Other\"=dword:00000001"));
        assert!(updated.contains("\"OverlayPath\"=\"C:/obelisk/EOSOverlay\""));
        assert!(!updated.contains("Z:/host/old"));
        assert_eq!(registry_with_overlay(&updated), updated);
        assert_eq!(
            updated.matches("[Software\\\\Epic Games\\\\EOS]").count(),
            1
        );
        let fresh = registry_with_overlay("WINE REGISTRY Version 2\n");
        assert_eq!(registry_with_overlay(&fresh), fresh);
    }
}
