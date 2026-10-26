//! Pets que vêm dentro do exe. Só entram aqui pets que temos direito de distribuir
//! (originais do projeto). Cada um é instalado em `pets_dir` uma única vez: se o usuário apagar
//! a pasta depois, o app não recria. Uma versão nova do embutido só substitui a pasta que ainda
//! está igual a alguma versão que o app instalou; pet editado pelo usuário fica como está.

use std::collections::BTreeMap;
use std::path::Path;

pub struct Builtin {
    pub id: &'static str,
    files: &'static [(&'static str, &'static [u8])],
    /// Impressões digitais (`fingerprint`) das versões publicadas antes desta. Servem para quem
    /// instalou o pet antes de o config guardar a impressão da versão instalada.
    previous: &'static [&'static str],
}

pub const DEFAULT_PET: &str = "tatu-canastra";

const SHEET_FILE: &str = "spritesheet.webp";

/// Um pet de `assets/pets/<id>/`, no formato do Codex (`pet.json` + `spritesheet.webp`).
macro_rules! builtin {
    ($id:literal, previous: $previous:expr) => {
        Builtin {
            id: $id,
            // O pet.json vai por último: é ele que marca o pet como instalado, então uma
            // escrita interrompida no spritesheet não deixa um pet quebrado "instalado".
            files: &[
                (
                    SHEET_FILE,
                    include_bytes!(concat!("../assets/pets/", $id, "/spritesheet.webp")),
                ),
                (
                    crate::pets::MANIFEST_FILE,
                    include_bytes!(concat!("../assets/pets/", $id, "/pet.json")),
                ),
            ],
            previous: $previous,
        }
    };
}

pub const BUILTIN: &[Builtin] = &[
    builtin!(
        "tatu-canastra",
        // v0.3.0 a v0.4.0, antes da marca da SIMPIPE na carapaça.
        previous: &["0bddf5968dd45e4372bdc8ebf88be242e22131af9f3a3c00006d41fdd8b34e1f"]
    ),
    builtin!("draco", previous: &[]),
];

/// SHA-256 de `pet.json` + NUL + `spritesheet.webp`. O `\r` do pet.json sai antes: o checkout do
/// git no Windows pode ter convertido as quebras de linha no build que o usuário recebeu.
fn fingerprint(manifest: &[u8], sheet: &[u8]) -> Option<String> {
    let mut data: Vec<u8> = manifest.iter().copied().filter(|&b| b != b'\r').collect();
    data.push(0);
    data.extend_from_slice(sheet);
    crate::net::sha256_hex(&data).ok()
}

fn local_fingerprint(dir: &Path) -> Option<String> {
    let manifest = std::fs::read(dir.join(crate::pets::MANIFEST_FILE)).ok()?;
    let sheet = std::fs::read(dir.join(SHEET_FILE)).ok()?;
    fingerprint(&manifest, &sheet)
}

impl Builtin {
    fn file(&self, name: &str) -> &'static [u8] {
        self.files
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(&[], |(_, bytes)| *bytes)
    }

    fn fingerprint(&self) -> Option<String> {
        fingerprint(self.file(crate::pets::MANIFEST_FILE), self.file(SHEET_FILE))
    }

    fn write(&self, dir: &Path) -> bool {
        std::fs::create_dir_all(dir).is_ok()
            && self.files.iter().all(|(name, bytes)| {
                // Why: tmp + rename para o watcher de pastas nunca ler um arquivo pela metade.
                let tmp = dir.join(format!("{name}.tmp"));
                std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, dir.join(name)).is_ok()
            })
    }
}

/// Instala os embutidos que ainda não foram instalados e atualiza os que continuam iguais a uma
/// versão que o app instalou. `installed` mapeia id → impressão da versão gravada (vazia quando
/// veio de um config antigo, que só guardava o id). Devolve se mudou algo a salvar.
pub fn sync(pets_dir: &Path, installed: &mut BTreeMap<String, String>) -> bool {
    let mut changed = false;
    for pet in BUILTIN {
        let Some(current) = pet.fingerprint() else {
            continue;
        };
        let dir = pets_dir.join(pet.id);
        let exists = dir.join(crate::pets::MANIFEST_FILE).is_file();
        let known = match installed.get(pet.id) {
            Some(known) if *known == current => continue,
            Some(known) => known.clone(),
            None => {
                if exists {
                    // O usuário tinha o pet antes do app: só registra, sem sobrescrever.
                    installed.insert(pet.id.to_string(), String::new());
                    changed = true;
                } else if pet.write(&dir) {
                    installed.insert(pet.id.to_string(), current);
                    changed = true;
                }
                continue;
            }
        };
        // Apagado pelo usuário: continua apagado.
        if !exists {
            continue;
        }
        let Some(local) = local_fingerprint(&dir) else {
            continue;
        };
        let ours = local == current
            || (!known.is_empty() && local == known)
            || pet.previous.contains(&local.as_str());
        if ours && (local == current || pet.write(&dir)) {
            installed.insert(pet.id.to_string(), current);
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("pets-builtin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn tatu() -> &'static Builtin {
        BUILTIN.iter().find(|b| b.id == DEFAULT_PET).unwrap()
    }

    /// Grava uma versão "antiga" do tatu na pasta e devolve a impressão dela.
    fn old_version(root: &Path) -> String {
        let dir = root.join(DEFAULT_PET);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pet.json"), br#"{"id":"tatu-canastra"}"#).unwrap();
        std::fs::write(dir.join(SHEET_FILE), b"velho").unwrap();
        local_fingerprint(&dir).unwrap()
    }

    #[test]
    fn builtin_pets_are_valid_codex_pets() {
        let root = temp_root("valid");
        let mut installed = BTreeMap::new();
        assert!(sync(&root, &mut installed));
        assert!(installed.contains_key(DEFAULT_PET));
        assert_eq!(installed.len(), BUILTIN.len());
        for pet in BUILTIN {
            let manifest = crate::pets::read_manifest(&root.join(pet.id)).unwrap();
            assert_eq!(manifest.id.as_deref(), Some(pet.id), "id do pet.json");
            let sprite = crate::sprite::Sprite::build(&root.join(pet.id), 160).unwrap();
            assert!(
                sprite.has("idle") && sprite.has("running") && sprite.has("waiting"),
                "{}",
                pet.id
            );
        }

        // Segunda vez não muda nada, e uma pasta apagada pelo usuário continua apagada.
        std::fs::remove_dir_all(root.join(DEFAULT_PET)).unwrap();
        assert!(!sync(&root, &mut installed));
        assert!(!root.join(DEFAULT_PET).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn updates_an_unedited_builtin() {
        let root = temp_root("update");
        let old = old_version(&root);
        let mut installed = BTreeMap::from([(DEFAULT_PET.to_string(), old)]);
        assert!(sync(&root, &mut installed));
        assert_eq!(installed.get(DEFAULT_PET), tatu().fingerprint().as_ref());
        assert_eq!(
            local_fingerprint(&root.join(DEFAULT_PET)),
            tatu().fingerprint()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn keeps_an_edited_builtin() {
        let root = temp_root("edited");
        let old = old_version(&root);
        let edited = br#"{"id":"tatu-canastra","displayName":"Meu tatu"}"#;
        std::fs::write(root.join(DEFAULT_PET).join("pet.json"), edited).unwrap();
        let mut installed = BTreeMap::from([(DEFAULT_PET.to_string(), old.clone())]);
        sync(&root, &mut installed);
        assert_eq!(installed[DEFAULT_PET], old);
        assert_eq!(
            std::fs::read(root.join(DEFAULT_PET).join("pet.json")).unwrap(),
            edited
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn old_config_only_updates_a_known_previous_version() {
        // Config antigo (sem impressão) e conteúdo que não é nenhuma versão publicada: não mexe.
        let root = temp_root("legacy");
        old_version(&root);
        let mut installed = BTreeMap::from([(DEFAULT_PET.to_string(), String::new())]);
        sync(&root, &mut installed);
        assert_eq!(installed[DEFAULT_PET], "");
        assert_eq!(
            std::fs::read(root.join(DEFAULT_PET).join(SHEET_FILE)).unwrap(),
            b"velho"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fingerprint_ignores_crlf_in_the_manifest() {
        assert_eq!(fingerprint(b"{\r\n}", b"x"), fingerprint(b"{\n}", b"x"));
    }
}
