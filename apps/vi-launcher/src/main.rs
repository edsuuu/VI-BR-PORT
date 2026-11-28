//! The launcher of the fork: a window where the player picks the folder of
//! their own copy of the game (checked for the title's executable), sets the
//! options and any other `VICTORY_*` switch, and plays. The steps the first run
//! of a build needs (translating the shaders, warming the pipelines) run from
//! here, and both the launcher and the build it starts are kept up to date
//! from the fork's GitHub Pages manifest, with its GitHub releases as the
//! fallback.
//!
//! With arguments it prints instead of opening the window:
//! `vi-launcher --check <folder>` checks a game folder, `--version` prints the
//! version, `--updates` asks both sources for a newer release.
//!
//! The engine is linked in: started with one of the engine's commands (`run`,
//! `shaders`, `warm`, ...; `victory_app::is_engine_command`) this executable is
//! VIctory itself, so a release is this one file. The launcher starts the game
//! by starting itself with those commands ([`paths::find_victory`]), and the
//! engine's own copies of itself (warm workers, a relaunch) come back here too.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod controls;
mod flags;
mod game;
mod hardware;
mod mark;
mod options;
mod paths;
mod run;
mod text;
mod theme;
mod tray;
mod ui;
mod update;

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    if victory_app::is_engine_command() {
        return engine();
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return ui::start();
    }
    attach_console();
    match cli(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// The engine, as the `victory` binary runs it (Windows; elsewhere the launcher
/// starts `victory.exe` through Wine). Its output goes where the one who
/// started it sent it (the launcher's log pipe); started by hand from a console,
/// it prints there.
#[cfg(windows)]
fn engine() -> ExitCode {
    if !stdout_redirected() {
        attach_console();
    }
    match victory_app::main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:?}");
            ExitCode::FAILURE
        }
    }
}

/// Whether this process was given an output to write to (a pipe or a file).
#[cfg(windows)]
fn stdout_redirected() -> bool {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    // SAFETY: no pointers.
    let h = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    !h.is_null() && h != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE
}


/// The commands that print instead of opening the window.
fn cli(args: &[String]) -> Result<(), String> {
    let language = text::Language::detect(None);
    match args[0].as_str() {
        "--version" | "-V" => {
            println!("vi-launcher {}", update::current_version());
            Ok(())
        }
        "--check" => {
            let folder = args.get(1).ok_or("--check <game folder>")?;
            match game::find_game_exe(std::path::Path::new(folder)) {
                Ok(exe) => {
                    println!("{}", text::get(language, "game.ok").replace("{0}", &exe.display().to_string()));
                    Ok(())
                }
                Err(problem) => Err(problem.describe(language)),
            }
        }
        "--updates" => {
            let found = update::check(&update::Sources::fork()).map_err(|e| e.to_string())?;
            match found {
                Some(release) => println!("{} ({})", release.manifest.version, release.source),
                None => println!("{}", text::get(language, "update.none")),
            }
            Ok(())
        }
        other => Err(format!("unknown argument {other}: --version, --check <folder>, --updates")),
    }
}

/// A release build has no console of its own: print into the one it was
/// started from, if any.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: no pointers; failing (no parent console) leaves the output unseen.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}
