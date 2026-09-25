use super::*;

// Depot 228989 of Steamworks Common Redistributables, as Valve ships it.
const VCREDIST_2022: &str = r#""installscript"
{
	"Run Process"
	{
		"x86 14.51.36247.0"
		{
			"hasrunkey"		"HKEY_LOCAL_MACHINE\\Software\\Valve\\Steam\\Apps\\CommonRedist\\vcredist\\2022"
			"process 1"		"%INSTALLDIR%\\_CommonRedist\\vcredist\\2022\\Microsoft Visual C++ 2022 x86.cmd"
			"nocleanup"		"1"
		}
		"x64 14.51.36247.0"
		{
			"hasrunkey"		"HKEY_LOCAL_MACHINE\\Software\\Valve\\Steam\\Apps\\CommonRedist\\vcredist\\2022"
			"process 1"		"%INSTALLDIR%\\_CommonRedist\\vcredist\\2022\\Microsoft Visual C++ 2022 x64.cmd"
			"nocleanup"		"1"
			"Requirement_OS"
			{
				"Is64BitWindows"		"1"
			}
		}
	}
}
"kvsignatures"
{
	"installscript"		"37b956c5"
}
"#;

fn install() -> PathBuf {
    PathBuf::from("/Games/steam-526870-windows")
}

#[test]
fn a_64_bit_machine_runs_both_visual_cpp_steps_once_each() {
    let steps = steps(VCREDIST_2022.as_bytes(), &install(), "Z:\\Games").unwrap();
    let key = "HKEY_LOCAL_MACHINE\\Software\\Valve\\Steam\\Apps\\CommonRedist\\vcredist\\2022";
    assert_eq!(
        steps,
        [
            Step {
                id: format!("{key}\\x86 14.51.36247.0"),
                programs: vec![Program {
                    path: install()
                        .join("_CommonRedist/vcredist/2022/Microsoft Visual C++ 2022 x86.cmd"),
                    arguments: String::new(),
                }],
            },
            Step {
                id: format!("{key}\\x64 14.51.36247.0"),
                programs: vec![Program {
                    path: install()
                        .join("_CommonRedist/vcredist/2022/Microsoft Visual C++ 2022 x64.cmd"),
                    arguments: String::new(),
                }],
            },
        ]
    );
}

#[test]
fn programs_keep_their_order_and_command_lines_name_the_windows_installation() {
    let script = r#""InstallScript" { "Run Process" { "setup" {
        "process 1" "%InstallDir%\\redist\\first.exe"
        "command 1" "/silent /log=%INSTALLDIR%\\first.log"
        "process 2" "%INSTALLDIR%\\redist\\second.exe"
    } } }"#;
    let steps = steps(script.as_bytes(), &install(), "Z:\\Games\\Game").unwrap();
    assert_eq!(steps[0].id, "setup");
    assert_eq!(
        steps[0].programs,
        [
            Program {
                path: install().join("redist/first.exe"),
                arguments: "/silent /log=Z:\\Games\\Game\\first.log".into(),
            },
            Program {
                path: install().join("redist/second.exe"),
                arguments: String::new(),
            },
        ]
    );
}

#[test]
fn steps_for_other_machines_or_outside_the_installation_are_left_out() {
    let script = r#""installscript" { "Run Process" {
        "32-bit only" { "process 1" "%INSTALLDIR%\\a.exe" "Requirement_OS" { "Is64BitWindows" "0" } }
        "Windows XP" { "process 1" "%INSTALLDIR%\\b.exe" "Requirement_OS" { "OSType" "WinXP" } }
        "system" { "process 1" "%WINDIR%\\system32\\c.exe" }
        "escape" { "process 1" "%INSTALLDIR%\\..\\d.exe" }
        "nothing" { "nocleanup" "1" }
    } }"#;
    assert_eq!(steps(script.as_bytes(), &install(), "Z:\\").unwrap(), []);
}

#[test]
fn utf16_scripts_and_scripts_without_steps_are_read() {
    let mut utf16 = vec![0xff, 0xfe];
    for unit in VCREDIST_2022.encode_utf16() {
        utf16.extend(unit.to_le_bytes());
    }
    assert_eq!(steps(&utf16, &install(), "Z:\\").unwrap().len(), 2);
    let registry = r#""installscript" { "Registry" { "HKEY_LOCAL_MACHINE\\Software\\Game" { } } }"#;
    assert_eq!(steps(registry.as_bytes(), &install(), "Z:\\").unwrap(), []);
    assert!(steps(b"\"installscript\" {", &install(), "Z:\\").is_err());
}
