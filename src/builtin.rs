//! Pets que vêm dentro do exe. Só entram aqui pets que temos direito de distribuir
//! (originais do projeto). Cada um é instalado em `pets_dir` uma única vez: se o usuário editar
//! ou apagar a pasta depois, o app não recria nem sobrescreve.

use std::path::Path;

pub struct Builtin {
    pub id: &'static str,
    files: &'static [(&'static str, &'static [u8])],
}

pub const DEFAULT_PET: &str = "tatu-canastra";

/// Um pet de `assets/pets/<id>/`, no formato do Codex (`pet.json` + `spritesheet.webp`).
macro_rules! builtin {
    ($id:literal) => {
        Builtin {
            id: $id,
            files: &[
                (
                    "pet.json",
                    include_bytes!(concat!("../assets/pets/", $id, "/pet.json")),
                ),
                (
                    "spritesheet.webp",
                    include_bytes!(concat!("../assets/pets/", $id, "/spritesheet.webp")),
                ),
            ],
        }
    };
}

pub const BUILTIN: &[Builtin] = &[builtin!("tatu-canastra"), builtin!("fluxo")];

/// Instala os embutidos que ainda não foram instalados. `done` são os ids já instalados antes
/// (vêm do config); devolve os que entraram agora.
pub fn install_missing(pets_dir: &Path, done: &[String]) -> Vec<String> {
    let mut installed = Vec::new();
    for pet in BUILTIN {
        if done.iter().any(|d| d == pet.id) {
            continue;
        }
        let dir = pets_dir.join(pet.id);
        // Já existe (o usuário tinha o pet antes): só registra, sem sobrescrever.
        if dir.join(crate::pets::MANIFEST_FILE).is_file() {
            installed.push(pet.id.to_string());
            continue;
        }
        let ok = std::fs::create_dir_all(&dir).is_ok()
            && pet
                .files
                .iter()
                .all(|(name, bytes)| std::fs::write(dir.join(name), bytes).is_ok());
        if ok {
            installed.push(pet.id.to_string());
        }
    }
    installed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_pets_are_valid_codex_pets() {
        let root = std::env::temp_dir().join(format!("pets-builtin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let all: Vec<String> = BUILTIN.iter().map(|b| b.id.to_string()).collect();
        assert!(all.iter().any(|id| id == DEFAULT_PET));
        assert_eq!(install_missing(&root, &[]), all);
        for id in &all {
            let manifest = crate::pets::read_manifest(&root.join(id)).unwrap();
            assert_eq!(manifest.id.as_deref(), Some(id.as_str()), "id do pet.json");
            let sprite = crate::sprite::Sprite::build(&root.join(id), 160).unwrap();
            assert!(
                sprite.has("idle") && sprite.has("running") && sprite.has("waiting"),
                "{id}"
            );
        }

        // Segunda vez não reinstala, e uma pasta apagada pelo usuário continua apagada.
        std::fs::remove_dir_all(root.join(DEFAULT_PET)).unwrap();
        assert!(install_missing(&root, &all).is_empty());
        assert!(!root.join(DEFAULT_PET).exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
