//! Validate storefront launch metadata without executing a process or a shell.
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchTarget {
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
    pub is_application_bundle: bool,
}

/// Storefront-neutral error suffix; callers supply their localization namespace.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct LaunchError(&'static str);

impl LaunchError {
    pub fn key_suffix(&self) -> &'static str {
        self.0
    }
}

/// A canonical installation directory selected by the caller.
/// Validating it separately preserves storefront-specific configuration ordering.
pub struct LaunchDirectory(PathBuf);

impl LaunchDirectory {
    pub fn new(directory: &Path) -> Result<Self, LaunchError> {
        if !directory.is_absolute() || !directory.is_dir() {
            return Err(LaunchError("directoryMissing"));
        }
        directory
            .canonicalize()
            .map(Self)
            .map_err(|_| LaunchError("directoryUnreadable"))
    }

    pub fn resolve(
        &self,
        platform: &str,
        executable: &str,
        launch_arguments: &str,
        working_directory: &str,
    ) -> Result<LaunchTarget, LaunchError> {
        validate_platform(platform)?;
        let relative = relative_path(executable)?;
        if relative.as_os_str().is_empty() {
            return Err(LaunchError("executableMissing"));
        }
        let executable = confined_existing(&self.0, &relative)?;
        let extension = executable
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");
        if platform == "macos"
            && ["exe", "com", "bat", "cmd"]
                .iter()
                .any(|ext| extension.eq_ignore_ascii_case(ext))
        {
            return Err(LaunchError("windowsExecutable"));
        }
        let bundle = if platform == "macos" {
            application_bundle(&executable)?
        } else {
            None
        };
        if bundle.is_none() && !executable.is_file() {
            return Err(LaunchError("notExecutable"));
        }
        let working_directory = confined_existing(&self.0, &relative_path(working_directory)?)?;
        if !working_directory.is_dir() {
            return Err(LaunchError("invalidWorkingDirectory"));
        }
        let is_application_bundle = bundle.is_some();
        let executable = bundle.unwrap_or(executable);
        Ok(LaunchTarget {
            executable: path_string(&executable)?,
            arguments: parse_arguments(launch_arguments, platform)?,
            working_directory: path_string(&working_directory)?,
            is_application_bundle,
        })
    }
}

pub fn resolve(
    platform: &str,
    directory: &Path,
    executable: &str,
    arguments: &str,
    working_directory: &str,
) -> Result<LaunchTarget, LaunchError> {
    LaunchDirectory::new(directory)?.resolve(platform, executable, arguments, working_directory)
}

fn validate_platform(platform: &str) -> Result<(), LaunchError> {
    if matches!(platform, "macos" | "windows") {
        Ok(())
    } else {
        Err(LaunchError("unsupportedPlatform"))
    }
}

fn relative_path(value: &str) -> Result<PathBuf, LaunchError> {
    // Storefront Windows metadata uses backslashes even when read on macOS.
    let normalized = value.replace('\\', "/");
    if normalized.contains(['\0', ':']) || normalized.starts_with('/') {
        return Err(LaunchError("invalidPath"));
    }
    let path = Path::new(&normalized);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(LaunchError("pathOutsideGame"));
    }
    Ok(path.to_owned())
}

fn confined_existing(root: &Path, relative: &Path) -> Result<PathBuf, LaunchError> {
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|_| LaunchError("fileMissing"))?;
    if !path.starts_with(root) {
        return Err(LaunchError("linkOutsideGame"));
    }
    Ok(path)
}

fn application_bundle(executable: &Path) -> Result<Option<PathBuf>, LaunchError> {
    if executable
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    {
        if executable.is_dir() {
            return Ok(Some(executable.to_owned()));
        }
        return Err(LaunchError("invalidApplication"));
    }
    // An explicit executable within a bundle may differ from CFBundleExecutable.
    // Preserve the publisher's exact target instead of substituting the bundle.
    Ok(None)
}

fn path_string(path: &Path) -> Result<String, LaunchError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(LaunchError("invalidUtf8Path"))
}

/// Split publisher arguments with platform quoting rules and no shell expansion.
pub fn parse_arguments(source: &str, platform: &str) -> Result<Vec<String>, LaunchError> {
    validate_platform(platform)?;
    if source.contains('\0') || source.len() > 65_536 {
        return Err(LaunchError("invalidArguments"));
    }
    if platform == "windows" {
        Ok(windows_arguments(source))
    } else {
        macos_arguments(source)
    }
}

fn windows_arguments(source: &str) -> Vec<String> {
    // Microsoft C runtime argv rules. Backslashes only escape double quotes;
    // no percent-variable expansion, wildcard expansion or command execution.
    let mut chars = source.chars().peekable();
    let mut output = Vec::new();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| matches!(c, ' ' | '\t')) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut argument = String::new();
        let mut quoted = false;
        while chars.peek().is_some() {
            if !quoted && chars.peek().is_some_and(|c| matches!(c, ' ' | '\t')) {
                break;
            }
            let mut backslashes = 0;
            while chars.peek() == Some(&'\\') {
                chars.next();
                backslashes += 1;
            }
            if chars.peek() == Some(&'"') {
                argument.extend(std::iter::repeat_n('\\', backslashes / 2));
                chars.next();
                if backslashes % 2 == 1 {
                    argument.push('"');
                } else if quoted && chars.peek() == Some(&'"') {
                    argument.push('"');
                    chars.next();
                } else {
                    quoted = !quoted;
                }
            } else {
                argument.extend(std::iter::repeat_n('\\', backslashes));
                if !quoted && chars.peek().is_some_and(|c| matches!(c, ' ' | '\t')) {
                    break;
                }
                if let Some(character) = chars.next() {
                    argument.push(character);
                }
            }
        }
        output.push(argument);
    }
    output
}

fn macos_arguments(source: &str) -> Result<Vec<String>, LaunchError> {
    let mut chars = source.chars().peekable();
    let mut output = Vec::new();
    let mut argument = String::new();
    let mut quote = None;
    let mut started = false;
    while let Some(character) = chars.next() {
        match (quote, character) {
            (None, character) if character.is_ascii_whitespace() => {
                if started {
                    output.push(std::mem::take(&mut argument));
                    started = false;
                }
            }
            (None, '\'' | '"') => {
                quote = Some(character);
                started = true;
            }
            (Some(current), character) if character == current => quote = None,
            (None | Some('"'), '\\') => {
                let escaped = chars.next().ok_or(LaunchError("incompleteEscape"))?;
                if escaped == '\n' {
                    continue;
                }
                if quote == Some('"') && !matches!(escaped, '$' | '`' | '"' | '\\') {
                    argument.push('\\');
                }
                argument.push(escaped);
                started = true;
            }
            _ => {
                argument.push(character);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(LaunchError("unclosedQuotes"));
    }
    if started {
        output.push(argument);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(root: &Path, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"fixture, never executed").unwrap();
    }

    #[test]
    fn resolves_bundles_and_explicit_executables_without_changing_the_target() {
        let root = tempfile::tempdir().unwrap();
        file(root.path(), "My Game.app/Contents/MacOS/custom-executable");
        let bundle = resolve(
            "macos",
            root.path(),
            "My Game.app",
            "--title 'My game'",
            "My Game.app/Contents",
        )
        .unwrap();
        assert!(bundle.is_application_bundle);
        assert!(bundle.executable.ends_with("/My Game.app"));
        assert_eq!(bundle.arguments, ["--title", "My game"]);
        assert!(bundle.working_directory.ends_with("/My Game.app/Contents"));
        let explicit = resolve(
            "macos",
            root.path(),
            "My Game.app/Contents/MacOS/custom-executable",
            "",
            "",
        )
        .unwrap();
        assert!(!explicit.is_application_bundle);
        assert!(
            explicit
                .executable
                .ends_with("/Contents/MacOS/custom-executable")
        );
        assert_eq!(
            explicit.working_directory,
            root.path().canonicalize().unwrap().to_str().unwrap()
        );
        file(root.path(), "bin/game.exe");
        let windows = resolve("windows", root.path(), r"bin\game.exe", "", r"bin\").unwrap();
        assert!(!windows.is_application_bundle);
        assert!(windows.executable.ends_with("/bin/game.exe"));
    }

    #[test]
    fn rejects_missing_escaping_and_incompatible_launch_paths_with_stable_suffixes() {
        let root = tempfile::tempdir().unwrap();
        file(root.path(), "game.exe");
        file(root.path(), "game");
        file(root.path(), "invalid.app");
        std::fs::create_dir(root.path().join("directory")).unwrap();
        for (executable, expected) in [
            ("", "executableMissing"),
            ("missing", "fileMissing"),
            ("../game", "pathOutsideGame"),
            (r"..\game", "pathOutsideGame"),
            ("/bin/sh", "invalidPath"),
            (r"C:\game.exe", "invalidPath"),
            (r"\\server\game", "invalidPath"),
            ("game.exe", "windowsExecutable"),
            ("invalid.app", "invalidApplication"),
            ("directory", "notExecutable"),
        ] {
            assert_eq!(
                resolve("macos", root.path(), executable, "", "")
                    .unwrap_err()
                    .key_suffix(),
                expected,
                "{executable}"
            );
        }
        assert_eq!(
            resolve("macos", root.path(), "game", "", "game")
                .unwrap_err()
                .key_suffix(),
            "invalidWorkingDirectory"
        );
        assert_eq!(
            resolve("linux", root.path(), "game", "", "")
                .unwrap_err()
                .key_suffix(),
            "unsupportedPlatform"
        );
        assert_eq!(
            resolve("macos", Path::new("relative"), "game", "", "")
                .unwrap_err()
                .key_suffix(),
            "directoryMissing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_executable_bundle_and_working_directory_symlink_escapes() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        file(outside.path(), "Game.app/Contents/MacOS/game");
        symlink(outside.path(), root.path().join("linked")).unwrap();
        for executable in ["linked/Game.app", "linked/Game.app/Contents/MacOS/game"] {
            assert_eq!(
                resolve("macos", root.path(), executable, "", "")
                    .unwrap_err()
                    .key_suffix(),
                "linkOutsideGame"
            );
        }
        file(root.path(), "game");
        assert_eq!(
            resolve("macos", root.path(), "game", "", "linked")
                .unwrap_err()
                .key_suffix(),
            "linkOutsideGame"
        );
    }

    #[test]
    fn rejects_invalid_argument_sources_and_preserves_quote_boundaries() {
        for platform in ["windows", "macos"] {
            for source in ["bad\0argument".to_owned(), "x".repeat(65_537)] {
                assert_eq!(
                    parse_arguments(&source, platform).unwrap_err().key_suffix(),
                    "invalidArguments"
                );
            }
        }
        assert_eq!(
            parse_arguments("'unfinished", "macos")
                .unwrap_err()
                .key_suffix(),
            "unclosedQuotes"
        );
        assert_eq!(
            parse_arguments("unfinished\\", "macos")
                .unwrap_err()
                .key_suffix(),
            "incompleteEscape"
        );
        assert_eq!(
            parse_arguments("", "linux").unwrap_err().key_suffix(),
            "unsupportedPlatform"
        );
    }

    #[test]
    fn splits_windows_arguments_using_microsoft_quoting_rules() {
        for (source, expected) in [
            (r#""a b c" d e"#, vec!["a b c", "d", "e"]),
            (r#""ab\"c" "\\" d"#, vec!["ab\"c", "\\", "d"]),
            (r#"a\\\b d"e f"g h"#, vec![r"a\\\b", "de fg", "h"]),
            (r#"a\\\"b c d"#, vec![r#"a\"b"#, "c", "d"]),
            (r#"a\\\\"b c" d e"#, vec![r"a\\b c", "d", "e"]),
            (r#"a"b"" c d"#, vec!["ab\" c d"]),
            (
                r#""" --path C:\Games\Fixture --literal %PATH%"#,
                vec!["", "--path", r"C:\Games\Fixture", "--literal", "%PATH%"],
            ),
            (r#"C:\Games\ next"#, vec![r"C:\Games\", "next"]),
        ] {
            assert_eq!(
                parse_arguments(source, "windows").unwrap(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn splits_macos_arguments_without_expanding_shell_syntax() {
        assert_eq!(
            parse_arguments(
                r#"--title 'My game' "" escaped\ space "$HOME" $(id) && * "a\qb" "a\"b""#,
                "macos"
            )
            .unwrap(),
            [
                "--title",
                "My game",
                "",
                "escaped space",
                "$HOME",
                "$(id)",
                "&&",
                "*",
                r"a\qb",
                "a\"b"
            ]
        );
        assert!(parse_arguments("\"unfinished", "macos").is_err());
        assert!(parse_arguments("unfinished\\", "macos").is_err());
        assert!(parse_arguments("contains\0nul", "macos").is_err());
    }
}
