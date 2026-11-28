//! `victory`: the command-line front end (the code is in the library, so that
//! the launcher's executable can hold the same engine).

fn main() -> anyhow::Result<()> {
    victory_app::main()
}
