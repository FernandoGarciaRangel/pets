// Em release não há console: o pet vive na bandeja, e o modo `hook` roda sem piscar janela.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod animation;
mod builtin;
mod catalog;
mod config;
mod hook;
mod install;
mod net;
mod pets;
mod size_panel;
mod sprite;
mod system;
mod tray;
mod update;
mod window;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hook") => hook::run(args.get(1).map(String::as_str).unwrap_or("unknown")),
        // O app dispara `install --from-update` sem console: erro vira caixa de mensagem.
        Some("install") if args.iter().any(|a| a == "--from-update") => {
            if let Err(e) = install::install_app() {
                system::message_box(&format!("A atualização falhou: {e}"), true);
            }
        }
        Some("install") => cli(install::install_app()),
        Some("check-update") => cli(match update::check() {
            update::CheckResult::Available(r) => Ok(format!(
                "v{} disponível (atual v{})",
                r.version,
                update::current_version()
            )),
            update::CheckResult::UpToDate => Ok(format!(
                "já na versão mais recente (v{})",
                update::current_version()
            )),
            update::CheckResult::Failed(e) => Err(e),
        }),
        Some("update") => cli(update::update_now()),
        Some("check-pets") => cli(Ok(catalog::report(&local_pets()))),
        Some("update-pets") => cli(catalog::update_all(&local_pets())),
        Some("install-hooks") => cli(install::install()),
        Some("uninstall-hooks") => cli(install::uninstall()),
        Some(other) if other.starts_with('-') || other == "help" => {
            system::attach_console();
            println!(
                "uso: pets [install | update | check-update | check-pets | update-pets | install-hooks | uninstall-hooks | hook <claude|codex>]"
            );
        }
        _ => window::run(),
    }
}

fn local_pets() -> Vec<pets::PetInfo> {
    pets::discover(&config::Config::load().pets_dir())
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
