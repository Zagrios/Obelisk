//! Steam install scripts: the redistributables, such as Visual C++ or DirectX,
//! Steam runs once on a Windows machine before a game's first launch.
use std::path::{Component, Path, PathBuf};

use crate::kv::{self, Kv};

/// The manifest flag of an install script file.
pub(crate) const FLAG: u32 = 256;
/// Where an installation lists, one relative path per line, the install
/// scripts its depots carry.
pub const LIST: &str = ".forge-content/install-scripts";
/// "process 1" through "process 16" in one step.
const MAX_PROGRAMS: usize = 16;

/// One program of a step, with its Windows command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub path: PathBuf,
    pub arguments: String,
}

/// One "Run Process" step. Steam records it under `HasRunKey` and runs it
/// once per machine, so `id` names both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub id: String,
    pub programs: Vec<Program>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct InstallScriptError(&'static str);

/// The steps of a 64-bit Windows machine. `%INSTALLDIR%` is `install` in
/// program paths, which must stay inside it, and `windows_install` in command
/// lines. A step with a requirement Forge cannot check is left out.
pub fn steps(
    script: &[u8],
    install: &Path,
    windows_install: &str,
) -> Result<Vec<Step>, InstallScriptError> {
    let root = kv::text(&utf8(script)?).map_err(|_| InstallScriptError("malformed"))?;
    let Some(processes) = root.at(&["installscript", "Run Process"]) else {
        return Ok(Vec::new());
    };
    let mut steps = Vec::new();
    'steps: for (name, step) in processes.children() {
        for (requirement, value) in step.get("Requirement_OS").map_or(&[][..], Kv::children) {
            if !requirement.eq_ignore_ascii_case("Is64BitWindows") || value.text() == Some("0") {
                continue 'steps;
            }
        }
        let mut programs = Vec::new();
        for index in 1..=MAX_PROGRAMS {
            let Some(path) = step.get(&format!("process {index}")).and_then(Kv::text) else {
                break;
            };
            let Some(path) = program_path(path, install) else {
                continue 'steps;
            };
            let arguments = step
                .get(&format!("command {index}"))
                .and_then(Kv::text)
                .unwrap_or_default();
            programs.push(Program {
                path,
                arguments: replace_install_dir(arguments, windows_install),
            });
        }
        if programs.is_empty() {
            continue;
        }
        let id = match step.get("HasRunKey").and_then(Kv::text) {
            Some(key) => format!("{key}\\{name}"),
            None => name.clone(),
        };
        steps.push(Step { id, programs });
    }
    Ok(steps)
}

/// Scripts come in UTF-8 or, from older tools, UTF-16 with a byte order mark.
fn utf8(script: &[u8]) -> Result<Vec<u8>, InstallScriptError> {
    let Some(units) = script.strip_prefix(&[0xff, 0xfe]) else {
        return Ok(script.to_vec());
    };
    let units: Vec<u16> = units
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16(&units)
        .map(String::into_bytes)
        .map_err(|_| InstallScriptError("invalid UTF-16"))
}

/// Only `%INSTALLDIR%` paths run: the installation's own files.
fn program_path(path: &str, install: &Path) -> Option<PathBuf> {
    let relative = strip_install_dir(path)?.replace('\\', "/");
    let relative = Path::new(relative.trim_start_matches('/'));
    relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
        .then(|| install.join(relative))
}

fn strip_install_dir(path: &str) -> Option<&str> {
    let prefix = path.get(..12)?;
    prefix
        .eq_ignore_ascii_case("%INSTALLDIR%")
        .then(|| &path[12..])
}

fn replace_install_dir(text: &str, windows_install: &str) -> String {
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.to_ascii_uppercase().find("%INSTALLDIR%") {
        result.push_str(&rest[..start]);
        result.push_str(windows_install);
        rest = &rest[start + 12..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests;
