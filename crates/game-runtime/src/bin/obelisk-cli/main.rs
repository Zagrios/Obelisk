//! obelisk-cli: what Obelisk does for one installed game, from a terminal,
//! for development, in the library `OBELISK_LIBRARY_DIRECTORY` names or else
//! the app's. `run` launches a GOG game through the same installations and
//! supervisor, so quit Obelisk first: both write the receipt book. Steam and
//! Epic need a session only Obelisk holds. A release build refuses to run.
mod resolve;
mod run;

use resolve::Installed;
use std::{
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const USAGE: &str = "\
usage: obelisk-cli [--app <Obelisk.app>] <command>
  run <game>                  launch a GOG game through its supervisor, as Obelisk does
  env <game>                  print the environment of the game's Wine
  prefix [--open] <game>      print the game's Wine prefix, or open it in Finder
  wine <game> <tool> [args]   run winecfg, regedit or winedbg in the game's prefix
  list                        list the installations
<game> is an installation's id, its store's id for the game, or its name or a part of it.
--app names the Obelisk build whose helpers run the game; run needs it.";

/// Where Obelisk keeps its library without `OBELISK_LIBRARY_DIRECTORY`.
const LIBRARY: &str = "Library/Application Support/com.zagrios.obelisk";

fn main() -> ExitCode {
    if !cfg!(debug_assertions) {
        eprintln!("obelisk-cli is a development tool: build it without --release");
        return ExitCode::from(2);
    }
    match command(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("obelisk-cli: {message}");
            ExitCode::FAILURE
        }
    }
}

/// What the command line asks.
#[derive(Debug, PartialEq, Eq)]
enum Asked {
    Run(String),
    Environment(String),
    Prefix {
        game: String,
        open: bool,
    },
    Wine {
        game: String,
        tool: game_runtime::wine::Tool,
        arguments: Vec<String>,
    },
    List,
}

/// The command line `arguments` and the app bundle `--app` names.
fn parse(mut arguments: Vec<String>) -> Result<(Asked, Option<PathBuf>), String> {
    let mut app = None;
    if let Some(at) = arguments.iter().position(|argument| argument == "--app") {
        if at + 1 >= arguments.len() {
            return Err(USAGE.into());
        }
        app = Some(PathBuf::from(arguments.remove(at + 1)));
        arguments.remove(at);
    }
    let strings: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let asked = match strings.as_slice() {
        ["run", game] => Asked::Run((*game).into()),
        ["env", game] => Asked::Environment((*game).into()),
        ["prefix", game] => Asked::Prefix {
            game: (*game).into(),
            open: false,
        },
        ["prefix", "--open", game] | ["prefix", game, "--open"] => Asked::Prefix {
            game: (*game).into(),
            open: true,
        },
        ["wine", game, tool, rest @ ..] => Asked::Wine {
            game: (*game).into(),
            tool: game_runtime::wine::Tool::ALL
                .into_iter()
                .find(|known| known.program() == *tool)
                .ok_or_else(|| format!("{tool} is no tool of Wine's this runs\n{USAGE}"))?,
            arguments: rest.iter().map(|argument| (*argument).to_owned()).collect(),
        },
        ["list"] => Asked::List,
        _ => return Err(USAGE.into()),
    };
    Ok((asked, app))
}

fn command(arguments: Vec<String>) -> Result<ExitCode, String> {
    let (asked, app) = parse(arguments)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let library = std::env::var_os("OBELISK_LIBRARY_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(LIBRARY));
    let bundle = app
        .clone()
        .unwrap_or_else(|| PathBuf::from("/Applications/Obelisk.app"));
    let locations = game_runtime::Locations::new(&library, &bundle, &home);
    let installed = resolve::installed(&library);
    if asked == Asked::List {
        for found in &installed {
            println!("{}", found.line());
        }
        return Ok(ExitCode::SUCCESS);
    }
    let wanted = match &asked {
        Asked::Run(game) | Asked::Environment(game) => game,
        Asked::Prefix { game, .. } | Asked::Wine { game, .. } => game,
        Asked::List => unreachable!(),
    };
    let found = resolve::find(&installed, wanted)?;
    match asked {
        Asked::Run(_) => {
            if app.is_none() {
                return Err("run needs --app <Obelisk.app>, whose helpers run the game".into());
            }
            run::run(found, &locations)
        }
        Asked::Environment(_) => {
            let windows = windows(found, &locations)?;
            let environment = windows.environment(&locations).map_err(launch_error)?;
            if windows.engine() == game_runtime::wine::Engine::Linux {
                println!("# inside the Linux engine's guest");
            }
            for (name, value) in environment {
                println!("{name}={}", quoted(&value));
            }
            Ok(ExitCode::SUCCESS)
        }
        Asked::Prefix { open, .. } => {
            let windows = windows(found, &locations)?;
            let prefix = windows.prefix();
            if !open {
                println!("{}", prefix.display());
                return Ok(ExitCode::SUCCESS);
            }
            if !prefix.is_dir() {
                return Err(format!(
                    "{} has no prefix yet: launch it once",
                    found.name()
                ));
            }
            status(Command::new("/usr/bin/open").arg(prefix))
        }
        Asked::Wine {
            tool, arguments, ..
        } => {
            let windows = windows(found, &locations)?;
            let Some(mut command) = windows.tool(&locations, tool).map_err(launch_error)? else {
                return Err(
                    "the Linux engine's Wine starts only with its game: no tool opens there".into(),
                );
            };
            status(command.args(arguments))
        }
        Asked::List => unreachable!(),
    }
}

/// The Windows installation of `found`, as a launch resolves it.
fn windows(
    found: &Installed,
    locations: &game_runtime::Locations,
) -> Result<game_runtime::wine::Installation, String> {
    found
        .windows(locations)
        .map_err(launch_error)?
        .ok_or_else(|| format!("{} is a Mac build: it runs without Wine", found.name()))
}

fn launch_error(error: game_runtime::launch::Error) -> String {
    match error {
        game_runtime::launch::Error::WineUnavailable => {
            "Voile is not installed: launch a Windows game in Obelisk once".into()
        }
        error => error.to_string(),
    }
}

/// Runs `command` in this terminal and exits as it did.
fn status(command: &mut Command) -> Result<ExitCode, String> {
    let status = command
        .status()
        .map_err(|error| format!("{}: {error}", Path::new(command.get_program()).display()))?;
    Ok(ExitCode::from(
        status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1),
    ))
}

/// `value` as a shell reads it back.
fn quoted(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.,/:=+@%".contains(c));
    if plain {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game_runtime::wine::Tool;

    fn parsed(line: &str) -> Result<(Asked, Option<PathBuf>), String> {
        parse(line.split(' ').map(str::to_owned).collect())
    }

    #[test]
    fn the_command_line_names_one_command_its_game_and_the_app() {
        assert_eq!(
            parsed("--app /Apps/Obelisk.app run 1207658924").unwrap(),
            (
                Asked::Run("1207658924".into()),
                Some(PathBuf::from("/Apps/Obelisk.app"))
            )
        );
        assert_eq!(
            parsed("prefix witcher --open").unwrap().0,
            Asked::Prefix {
                game: "witcher".into(),
                open: true
            }
        );
        assert_eq!(
            parsed("wine witcher regedit /E out.reg").unwrap().0,
            Asked::Wine {
                game: "witcher".into(),
                tool: Tool::Registry,
                arguments: vec!["/E".into(), "out.reg".into()]
            }
        );
        assert_eq!(parsed("env 553850 --app").unwrap_err(), USAGE.to_owned());
        assert!(
            parsed("wine witcher notepad")
                .unwrap_err()
                .contains("notepad")
        );
        assert!(parsed("run").is_err());
    }

    #[test]
    fn values_print_as_a_shell_reads_them_back() {
        assert_eq!(quoted("/Users/me/Prefixes/game"), "/Users/me/Prefixes/game");
        assert_eq!(
            quoted("mshtml=;winemenubuilder.exe=d"),
            "'mshtml=;winemenubuilder.exe=d'"
        );
        assert_eq!(quoted("it's"), r"'it'\''s'");
        assert_eq!(quoted(""), "''");
    }
}
