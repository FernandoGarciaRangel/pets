// Em release não há console: o pet vive na bandeja, e o modo `hook` roda sem piscar janela.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod animation;
mod config;
mod hook;
mod install;
mod pets;
mod sprite;
mod system;
mod tray;
mod window;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hook") => hook::run(args.get(1).map(String::as_str).unwrap_or("unknown")),
        Some("install") => cli(install::install_app()),
        Some("install-hooks") => cli(install::install()),
        Some("uninstall-hooks") => cli(install::uninstall()),
        Some(other) if other.starts_with('-') || other == "help" => {
            system::attach_console();
            println!("uso: pets [install | install-hooks | uninstall-hooks | hook <claude|codex>]");
        }
        _ => window::run(),
    }
}

fn cli(result: Result<String, String>) {
    system::attach_console();
    match result {
        Ok(message) => println!("{message}"),
        Err(error) => {
            eprintln!("erro: {error}");
            std::process::exit(1);
        }
    }
}
