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
//! The engine is its own executable (victory.exe), released apart: the launcher
//! finds it next to itself, in a checkout it builds, or where the settings say.
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


/// The commands that print instead of opening the window.
fn cli(args: &[String]) -> Result<(), String> {
    let language = text::Language::detect(None);
    match args[0].as_str() {
        "--version" | "-V" => {
            println!("ED_Launcher {}", update::current_version());
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
        "--settings" => {
            let path = paths::config_path();
            let loaded = config::load(&path);
            println!("{}", path.display());
            println!("game: {:?}", loaded.config.game_exe);
            println!("saves: {:?}", loaded.config.saves_dir);
            Ok(())
        }
        other => Err(format!("unknown argument {other}: --version, --check <folder>, --updates, --settings")),
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
