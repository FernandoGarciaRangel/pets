//! Atualizações dos pets a partir dos três catálogos: codex-pets.net, petdex.dev e
//! awesome-codex-pet (o repositório GitHub por trás do aimcp.info).
//!
//! Um pet só recebe atualização depois de identificado com exatidão: o spritesheet local precisa
//! ser idêntico ao de uma entrada do catálogo (mesmo tamanho, ou mesmo hash git no GitHub). Pets
//! criados localmente ou com o nome de outro pet ficam de fora, para nunca serem substituídos.

use crate::net;
use crate::pets::{self, PetInfo};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const CODEX_PETS: &str = "https://codex-pets.net";
const PETDEX_MANIFEST: &str = "https://assets.petdex.dev/manifests/petdex-v1.json";
const AWESOME_REPO: &str = "legeling/awesome-codex-pet";
const SHEET_FILE: &str = "spritesheet.webp";

/// Serializa leitura+escrita do `pet-sources.json` entre a checagem e a aplicação.
static STATE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    CodexPets,
    Petdex,
    AwesomeCodexPet,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::CodexPets => "codex-pets.net",
            Source::Petdex => "petdex.dev",
            Source::AwesomeCodexPet => "aimcp.info",
        }
    }
}

/// O que se sabe de cada pet local; persistido em `%APPDATA%\pets\pet-sources.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum Known {
    /// `key` é o id no catálogo e `version` o marcador da versão instalada. `size` é o tamanho
    /// do spritesheet local nesse momento: se mudar, o usuário editou o pet e ele não é mais
    /// atualizado (0 = estado antigo, sem essa informação).
    Tracked {
        source: Source,
        key: String,
        version: String,
        #[serde(default)]
        size: u64,
    },
    /// Não bateu com nenhum catálogo; só tenta de novo se o spritesheet mudar de tamanho.
    /// `seen`: o nome existe em algum catálogo, mas com outro spritesheet.
    Unmatched {
        size: u64,
        #[serde(default)]
        seen: bool,
    },
}

#[derive(Default, Serialize, Deserialize)]
struct State(BTreeMap<String, Known>);

impl State {
    fn path() -> PathBuf {
        crate::config::Config::path().with_file_name("pet-sources.json")
    }

    fn load() -> State {
        std::fs::read_to_string(Self::path())
            .ok()
            // Why: editores no Windows (e o Set-Content do PowerShell) gravam BOM, que o serde_json rejeita.
            .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let path = Self::path();
        if let (Some(dir), Ok(text)) = (path.parent(), serde_json::to_string_pretty(self)) {
            let _ = std::fs::create_dir_all(dir);
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

/// Como obter o `pet.json` da entrada.
#[derive(Debug, Clone, PartialEq)]
pub enum ManifestSource {
    Url(String),
    Inline(Map<String, Value>),
}

/// Uma entrada de catálogo como ela está agora.
#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    pub source: Source,
    pub key: String,
    pub version: String,
    pub sheet_url: String,
    /// Tamanho do spritesheet, quando o catálogo informa.
    pub sheet_size: Option<u64>,
    /// Hash git do spritesheet, quando o catálogo informa (GitHub).
    pub sheet_blob: Option<String>,
    pub manifest: ManifestSource,
}

#[derive(Debug, Clone)]
pub struct PetUpdate {
    pub id: String,
    pub label: String,
    pub remote: Remote,
}

/// Situação de cada pet depois de uma checagem; o CLI `check-pets` imprime isto.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    UpToDate(Source),
    UpdateAvailable(Source),
    /// Existe com o mesmo nome em algum catálogo, mas o spritesheet não é igual.
    Different,
    /// Foi identificado, mas o spritesheet local mudou depois: é do usuário agora.
    EditedLocally,
    NotInCatalogs,
    Error(String),
}

fn is_slug(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Marcador de versão do codex-pets: o `v` de `downloadUrl` (`?v=…`) ou de `/v/<n>/` no spritesheet.
fn codex_pets_version(pet: &Value) -> Option<String> {
    let from_query = pet["downloadUrl"].as_str().and_then(|u| {
        u.split_once('?')?
            .1
            .split('&')
            .find_map(|pair| pair.strip_prefix("v="))
    });
    let from_path = || {
        let url = pet["spritesheetUrl"].as_str()?;
        let after = url.split_once("/v/")?.1;
        after.split('/').next()
    };
    from_query
        .or_else(from_path)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| pet["uploadedAt"].as_str().map(str::to_string))
}

/// `share-data` do codex-pets; `None` quando o pet não existe lá.
pub fn parse_codex_pets(status: u32, body: &[u8]) -> Result<Option<Remote>, String> {
    if status == 404 {
        return Ok(None);
    }
    if status != 200 {
        return Err(format!("codex-pets.net respondeu HTTP {status}"));
    }
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("codex-pets.net: {e}"))?;
    let pet = &v["pet"];
    let Some(key) = pet["id"].as_str() else {
        return Ok(None);
    };
    let sheet_url = pet["spritesheetUrl"]
        .as_str()
        .ok_or("codex-pets.net: pet sem spritesheetUrl")?;
    let mut manifest = Map::new();
    for field in [
        "id",
        "displayName",
        "description",
        "spritesheetPath",
        "spriteVersionNumber",
        "kind",
    ] {
        if !pet[field].is_null() {
            manifest.insert(field.into(), pet[field].clone());
        }
    }
    Ok(Some(Remote {
        source: Source::CodexPets,
        key: key.to_string(),
        version: codex_pets_version(pet).ok_or("codex-pets.net: pet sem versão")?,
        sheet_url: sheet_url.to_string(),
        sheet_size: pet["validationReport"]["spritesheetBytes"].as_u64(),
        sheet_blob: None,
        manifest: ManifestSource::Inline(manifest),
    }))
}

/// Manifest público do petdex: uma lista com todos os pets aprovados.
pub fn parse_petdex(body: &[u8]) -> Result<Vec<Remote>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("petdex.dev: {e}"))?;
    let list = v["pets"]
        .as_array()
        .ok_or("petdex.dev: manifest sem pets")?;
    Ok(list
        .iter()
        .filter_map(|p| {
            let sheet = p["spritesheetUrl"].as_str()?;
            Some(Remote {
                source: Source::Petdex,
                key: p["slug"].as_str()?.to_string(),
                // A URL do spritesheet leva um hash do conteúdo; muda quando o autor atualiza.
                version: sheet.to_string(),
                sheet_url: sheet.to_string(),
                sheet_size: None,
                sheet_blob: None,
                manifest: ManifestSource::Url(p["petJsonUrl"].as_str()?.to_string()),
            })
        })
        .collect())
}

/// Árvore do repositório awesome-codex-pet: `pets/<id>--<autor>/spritesheet.webp`.
pub fn parse_awesome_tree(body: &[u8]) -> Result<Vec<Remote>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("aimcp.info: {e}"))?;
    // Why: com `truncated` a lista vem incompleta; tratar como completa marcaria pets como
    // "fora do catálogo" para sempre.
    if v["truncated"].as_bool() == Some(true) {
        return Err("aimcp.info: a árvore do GitHub veio truncada".into());
    }
    let tree = v["tree"]
        .as_array()
        .ok_or("aimcp.info: árvore sem entradas")?;
    let raw = format!("https://raw.githubusercontent.com/{AWESOME_REPO}/main/pets");
    Ok(tree
        .iter()
        .filter_map(|t| {
            let path = t["path"].as_str()?;
            let folder = path
                .strip_prefix("pets/")?
                .strip_suffix("/spritesheet.webp")?;
            if folder.contains('/') {
                return None;
            }
            let sha = t["sha"].as_str()?;
            Some(Remote {
                source: Source::AwesomeCodexPet,
                key: folder.to_string(),
                version: sha.to_string(),
                sheet_url: format!("{raw}/{folder}/spritesheet.webp"),
                sheet_size: t["size"].as_u64(),
                sheet_blob: Some(sha.to_string()),
                manifest: ManifestSource::Url(format!("{raw}/{folder}/pet.json")),
            })
        })
        .collect())
}

/// No awesome-codex-pet as pastas são `<id>--<autor>`; o pet local `goku` casa com `goku--fulano`.
fn awesome_matches(local_id: &str, folder: &str) -> bool {
    folder == local_id
        || folder
            .strip_prefix(local_id)
            .is_some_and(|rest| rest.starts_with("--"))
}

/// Índices grandes, baixados no máximo uma vez por checagem e só se algum pet precisar.
#[derive(Default)]
struct Catalogs {
    petdex: Option<Result<Vec<Remote>, String>>,
    awesome: Option<Result<Vec<Remote>, String>>,
}

impl Catalogs {
    fn petdex(&mut self) -> Result<&[Remote], String> {
        let entry = self
            .petdex
            .get_or_insert_with(|| net::get(PETDEX_MANIFEST).and_then(|b| parse_petdex(&b)));
        entry.as_deref().map_err(Clone::clone)
    }

    fn awesome(&mut self) -> Result<&[Remote], String> {
        let entry = self.awesome.get_or_insert_with(|| {
            let url =
                format!("https://api.github.com/repos/{AWESOME_REPO}/git/trees/main?recursive=1");
            net::get(&url).and_then(|b| parse_awesome_tree(&b))
        });
        entry.as_deref().map_err(Clone::clone)
    }
}

fn codex_pets_lookup(key: &str) -> Result<Option<Remote>, String> {
    if !is_slug(key) {
        return Ok(None);
    }
    let r = net::request("GET", &format!("{CODEX_PETS}/api/pets/{key}/share-data"))?;
    parse_codex_pets(r.status, &r.body)
}

fn lookup(catalogs: &mut Catalogs, source: Source, key: &str) -> Result<Option<Remote>, String> {
    match source {
        Source::CodexPets => codex_pets_lookup(key),
        Source::Petdex => Ok(catalogs.petdex()?.iter().find(|r| r.key == key).cloned()),
        Source::AwesomeCodexPet => Ok(catalogs.awesome()?.iter().find(|r| r.key == key).cloned()),
    }
}

/// Resultado de procurar um pet nos três catálogos.
struct Identified {
    remote: Option<Remote>,
    /// O nome existe em algum catálogo (para distinguir "diferente" de "não está em nenhum").
    seen: bool,
    /// Algum catálogo não respondeu: um "não achei" não é definitivo.
    failed: bool,
}

/// Procura a entrada cujo spritesheet é idêntico ao local.
fn identify(catalogs: &mut Catalogs, id: &str, sheet: &[u8]) -> Identified {
    let size = sheet.len() as u64;
    let mut found = Identified {
        remote: None,
        seen: false,
        failed: false,
    };
    match codex_pets_lookup(id) {
        Ok(Some(remote)) => {
            found.seen = true;
            if remote.sheet_size == Some(size) {
                found.remote = Some(remote);
                return found;
            }
        }
        Ok(None) => {}
        Err(_) => found.failed = true,
    }
    match catalogs.petdex() {
        Ok(list) => {
            let candidates: Vec<Remote> = list.iter().filter(|r| r.key == id).cloned().collect();
            for remote in candidates {
                found.seen = true;
                // O manifest do petdex não traz o tamanho; HEAD lê só o Content-Length.
                match net::request("HEAD", &remote.sheet_url) {
                    Ok(r) if r.status == 200 && r.content_length == Some(size) => {
                        found.remote = Some(remote);
                        return found;
                    }
                    // Tamanho diferente ou arquivo sumiu: não é este.
                    Ok(r) if (r.status == 200 && r.content_length.is_some()) || r.status == 404 => {
                    }
                    // Sem Content-Length (CDN com chunked/compressão), erro HTTP ou de rede.
                    _ => found.failed = true,
                }
            }
        }
        Err(_) => found.failed = true,
    }
    match catalogs.awesome() {
        Err(_) => found.failed = true,
        Ok(list) => {
            let candidates: Vec<&Remote> = list
                .iter()
                .filter(|r| awesome_matches(id, &r.key))
                .collect();
            if !candidates.is_empty() {
                found.seen = true;
                if let Ok(blob) = net::git_blob_sha1(sheet)
                    && let Some(remote) = candidates
                        .into_iter()
                        .find(|r| r.sheet_blob.as_deref() == Some(blob.as_str()))
                {
                    found.remote = Some(remote.clone());
                }
            }
        }
    }
    found
}

fn unmatched_status(seen: bool) -> Status {
    if seen {
        Status::Different
    } else {
        Status::NotInCatalogs
    }
}

fn local_sheet(pet: &PetInfo) -> Option<PathBuf> {
    let manifest = pets::read_manifest(&pet.dir).ok()?;
    pets::sheet_path(&pet.dir, &manifest).ok()
}

/// Checa todos os pets locais. Pets novos são identificados uma vez; os já identificados só
/// comparam a versão. Devolve a situação de cada um, na ordem de `pets`.
pub fn check(pets: &[PetInfo]) -> Vec<(PetInfo, Status, Option<Remote>)> {
    // Why: o lock cobre só ler e gravar o arquivo. A rede pode levar minutos (um pedido por pet,
    // timeouts de 30 s) e o `apply` de uma atualização não pode ficar esperando por ela.
    let state = {
        let _guard = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        State::load()
    };
    let mut changes: Vec<(String, Known)> = Vec::new();
    let mut catalogs = Catalogs::default();
    let mut out = Vec::new();

    for pet in pets {
        let Some(sheet_path) = local_sheet(pet) else {
            out.push((pet.clone(), Status::Error("pet.json inválido".into()), None));
            continue;
        };
        let size = std::fs::metadata(&sheet_path).map(|m| m.len()).unwrap_or(0);

        let result = match state.0.get(&pet.id).cloned() {
            Some(Known::Tracked { size: known, .. }) if known != 0 && known != size => {
                (Status::EditedLocally, None)
            }
            Some(Known::Tracked {
                source,
                key,
                version,
                size: known,
            }) => match lookup(&mut catalogs, source, &key) {
                Ok(Some(remote)) if remote.version == version => {
                    // Estado antigo sem tamanho: anota agora, para detectar edições daqui em diante.
                    if known == 0 {
                        changes.push((
                            pet.id.clone(),
                            Known::Tracked {
                                source,
                                key,
                                version,
                                size,
                            },
                        ));
                    }
                    (Status::UpToDate(source), None)
                }
                Ok(Some(remote)) => (Status::UpdateAvailable(source), Some(remote)),
                // Saiu do catálogo: mantém como está, sem atualização.
                Ok(None) => (Status::UpToDate(source), None),
                Err(e) => (Status::Error(e), None),
            },
            Some(Known::Unmatched { size: known, seen }) if known == size => {
                (unmatched_status(seen), None)
            }
            _ => {
                let Ok(sheet) = std::fs::read(&sheet_path) else {
                    out.push((
                        pet.clone(),
                        Status::Error("spritesheet ilegível".into()),
                        None,
                    ));
                    continue;
                };
                let found = identify(&mut catalogs, &pet.id, &sheet);
                match found.remote {
                    Some(remote) => {
                        changes.push((
                            pet.id.clone(),
                            Known::Tracked {
                                source: remote.source,
                                key: remote.key.clone(),
                                version: remote.version.clone(),
                                size,
                            },
                        ));
                        (Status::UpToDate(remote.source), None)
                    }
                    // Sem resposta de algum catálogo: não grava nada e tenta de novo na próxima.
                    None if found.failed => (
                        Status::Error(
                            "catálogo sem resposta; tenta de novo na próxima checagem".into(),
                        ),
                        None,
                    ),
                    None => {
                        changes.push((
                            pet.id.clone(),
                            Known::Unmatched {
                                size,
                                seen: found.seen,
                            },
                        ));
                        (unmatched_status(found.seen), None)
                    }
                }
            }
        };
        out.push((pet.clone(), result.0, result.1));
    }
    if !changes.is_empty() {
        // Relê antes de gravar: um `apply` pode ter registrado versão nova enquanto a rede rodava.
        let _guard = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut latest = State::load();
        for (id, after) in changes {
            if latest.0.get(&id) == state.0.get(&id) {
                latest.0.insert(id, after);
            }
        }
        latest.save();
    }
    out
}

pub fn updates(pets: &[PetInfo]) -> Vec<PetUpdate> {
    check(pets)
        .into_iter()
        .filter_map(|(pet, _, remote)| {
            Some(PetUpdate {
                id: pet.id,
                label: pet.label,
                remote: remote?,
            })
        })
        .collect()
}

/// Baixa a versão nova, valida numa pasta à parte, faz backup e só então troca os arquivos.
pub fn apply(pet_dir: &Path, update: &PetUpdate) -> Result<(), String> {
    let remote = &update.remote;
    let mut manifest = match &remote.manifest {
        ManifestSource::Inline(map) => map.clone(),
        ManifestSource::Url(url) => match serde_json::from_slice::<Value>(&net::get(url)?) {
            Ok(Value::Object(map)) => map,
            _ => return Err(format!("{}: pet.json inválido", remote.source.label())),
        },
    };
    // A pasta local continua com o mesmo nome e o mesmo id, para o Codex e o config não perderem o pet.
    manifest.insert("id".into(), json!(update.id));
    manifest.insert("spritesheetPath".into(), json!(SHEET_FILE));

    let sheet = net::get(&remote.sheet_url)?;
    if let Some(size) = remote.sheet_size
        && sheet.len() as u64 != size
    {
        return Err(format!(
            "spritesheet com tamanho errado ({} bytes, esperado {size})",
            sheet.len()
        ));
    }
    if let Some(blob) = &remote.sheet_blob
        && net::git_blob_sha1(&sheet)? != *blob
    {
        return Err("spritesheet não confere com o hash do GitHub".into());
    }

    let app_dir = crate::config::Config::path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let staging = app_dir.join("staging").join(&update.id);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let manifest_text =
        serde_json::to_string_pretty(&Value::Object(manifest)).map_err(|e| e.to_string())? + "\n";
    std::fs::write(staging.join(pets::MANIFEST_FILE), &manifest_text).map_err(|e| e.to_string())?;
    std::fs::write(staging.join(SHEET_FILE), &sheet).map_err(|e| e.to_string())?;
    // Decodifica e valida o layout antes de tocar no pet de verdade.
    crate::sprite::Cells::load(&staging).map_err(|e| format!("a versão nova não abre: {e}"))?;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = app_dir
        .join("backups")
        .join(format!("{}-{stamp}", update.id));
    std::fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
    if let Ok(entries) = std::fs::read_dir(pet_dir) {
        for entry in entries.flatten().filter(|e| e.path().is_file()) {
            std::fs::copy(entry.path(), backup.join(entry.file_name()))
                .map_err(|e| format!("backup falhou: {e}"))?;
        }
    }

    for name in [SHEET_FILE, pets::MANIFEST_FILE] {
        let tmp = pet_dir.join(format!("{name}.tmp-pets"));
        std::fs::copy(staging.join(name), &tmp).map_err(|e| e.to_string())?;
        // rename substitui o arquivo existente no Windows (MoveFileEx com REPLACE_EXISTING).
        std::fs::rename(&tmp, pet_dir.join(name))
            .map_err(|e| format!("não consegui trocar {name}: {e}"))?;
    }
    let _ = std::fs::remove_dir_all(&staging);

    let _guard = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = State::load();
    state.0.insert(
        update.id.clone(),
        Known::Tracked {
            source: remote.source,
            key: remote.key.clone(),
            version: remote.version.clone(),
            size: sheet.len() as u64,
        },
    );
    state.save();
    Ok(())
}

/// `pets.exe check-pets`: uma linha por pet.
pub fn report(pets: &[PetInfo]) -> String {
    let lines: Vec<String> = check(pets)
        .into_iter()
        .map(|(pet, status, _)| {
            let what = match status {
                Status::UpToDate(s) => format!("atualizado ({})", s.label()),
                Status::UpdateAvailable(s) => format!("VERSÃO NOVA em {}", s.label()),
                Status::Different => "diferente do catálogo (não atualiza)".into(),
                Status::EditedLocally => "editado localmente (não atualiza)".into(),
                Status::NotInCatalogs => "não está em nenhum catálogo".into(),
                Status::Error(e) => format!("erro: {e}"),
            };
            format!("{:28} {what}", pet.id)
        })
        .collect();
    lines.join("\n")
}

/// `pets.exe update-pets`: aplica todas as atualizações disponíveis.
pub fn update_all(pets: &[PetInfo]) -> Result<String, String> {
    let found = updates(pets);
    if found.is_empty() {
        return Ok("nenhum pet com versão nova".into());
    }
    let mut lines = Vec::new();
    for update in &found {
        let dir = pets
            .iter()
            .find(|p| p.id == update.id)
            .map(|p| p.dir.clone())
            .unwrap_or_default();
        lines.push(match apply(&dir, update) {
            Ok(()) => format!(
                "{}: atualizado ({})",
                update.label,
                update.remote.source.label()
            ),
            Err(e) => format!("{}: falhou — {e}", update.label),
        });
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_pets_share_data() {
        let body = br#"{"pet":{"id":"goku","displayName":"Goku","description":"d","spritesheetPath":"spritesheet.webp",
            "spriteVersionNumber":1,"kind":"person","ownerHandle":"x",
            "spritesheetUrl":"https://codex-pets.net/assets/pets/v/1777711765138/goku/spritesheet.webp",
            "downloadUrl":"/api/pets/goku/download?v=1777711765138",
            "validationReport":{"spritesheetBytes":1903110}}}"#;
        let r = parse_codex_pets(200, body).unwrap().unwrap();
        assert_eq!(r.version, "1777711765138");
        assert_eq!(r.sheet_size, Some(1903110));
        let ManifestSource::Inline(m) = &r.manifest else {
            panic!()
        };
        assert_eq!(
            m.keys().collect::<Vec<_>>(),
            [
                "id",
                "displayName",
                "description",
                "spritesheetPath",
                "spriteVersionNumber",
                "kind"
            ]
        );
        assert!(
            parse_codex_pets(404, br#"{"error":"pet not found"}"#)
                .unwrap()
                .is_none()
        );
        assert!(parse_codex_pets(500, b"").is_err());
    }

    #[test]
    fn codex_pets_version_reads_only_the_v_parameter() {
        let pet = json!({"downloadUrl":"/api/pets/x/download?rev=3&v=1777"});
        assert_eq!(codex_pets_version(&pet).as_deref(), Some("1777"));
        let pet = json!({"downloadUrl":"/api/pets/x/download?dev=1", "uploadedAt":"2026"});
        assert_eq!(codex_pets_version(&pet).as_deref(), Some("2026"));
    }

    #[test]
    fn codex_pets_version_falls_back_to_sheet_path() {
        let pet = json!({"spritesheetUrl":"https://x/assets/pets/v/42/a/spritesheet.webp"});
        assert_eq!(codex_pets_version(&pet).as_deref(), Some("42"));
    }

    #[test]
    fn parses_petdex_manifest() {
        let body = br#"{"generatedAt":"x","total":1,"pets":[{"slug":"boba","spritesheetUrl":"https://assets.petdex.dev/pets/boba-ab12/sprite.webp","petJsonUrl":"https://assets.petdex.dev/pets/boba-ab12/petjson.json"}]}"#;
        let list = parse_petdex(body).unwrap();
        assert_eq!(list[0].key, "boba");
        assert_eq!(list[0].version, list[0].sheet_url);
    }

    #[test]
    fn parses_awesome_tree_and_matches_author_folders() {
        let body = br#"{"tree":[
            {"path":"pets/kid-goku--julianhuang/spritesheet.webp","sha":"abc","size":10},
            {"path":"pets/kid-goku--julianhuang/pet.json","sha":"def","size":1},
            {"path":"pets/x/y/spritesheet.webp","sha":"zzz"}]}"#;
        let list = parse_awesome_tree(body).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].sheet_blob.as_deref(), Some("abc"));
        assert!(
            list[0]
                .sheet_url
                .ends_with("/pets/kid-goku--julianhuang/spritesheet.webp")
        );
        assert!(awesome_matches("kid-goku", "kid-goku--julianhuang"));
        assert!(awesome_matches("kid-goku", "kid-goku"));
        assert!(!awesome_matches("kid", "kid-goku--julianhuang"));
    }

    #[test]
    fn state_round_trips() {
        let mut s = State::default();
        s.0.insert(
            "goku".into(),
            Known::Tracked {
                source: Source::CodexPets,
                key: "goku".into(),
                version: "1".into(),
                size: 10,
            },
        );
        s.0.insert(
            "fluxo".into(),
            Known::Unmatched {
                size: 5,
                seen: true,
            },
        );
        let text = serde_json::to_string(&s).unwrap();
        assert!(
            text.contains(r#""status":"tracked""#) && text.contains(r#""source":"codex-pets""#)
        );
        let back: State = serde_json::from_str(&text).unwrap();
        assert_eq!(back.0, s.0);
        // Estados gravados antes dos campos `seen` e `size` ainda carregam.
        let tracked: State = serde_json::from_str(
            r#"{"g":{"status":"tracked","source":"petdex","key":"g","version":"1"}}"#,
        )
        .unwrap();
        assert!(matches!(tracked.0["g"], Known::Tracked { size: 0, .. }));
        let old: State = serde_json::from_str(r#"{"x":{"status":"unmatched","size":1}}"#).unwrap();
        assert_eq!(
            old.0["x"],
            Known::Unmatched {
                size: 1,
                seen: false
            }
        );
    }

    /// Rede: percorre identificar → versão antiga → atualizar com um pet do awesome-codex-pet.
    /// `cargo test awesome_codex_pet_end_to_end -- --ignored`
    #[test]
    #[ignore = "usa a rede e o pet-sources.json real"]
    fn awesome_codex_pet_end_to_end() {
        let id = "amadeus--legeling";
        let root = std::env::temp_dir().join(format!("pets-e2e-{}", std::process::id()));
        let pet_dir = root.join(id);
        std::fs::create_dir_all(&pet_dir).unwrap();
        let raw = format!("https://raw.githubusercontent.com/{AWESOME_REPO}/main/pets/{id}");
        for file in [pets::MANIFEST_FILE, SHEET_FILE] {
            let bytes = net::get(&format!("{raw}/{file}")).unwrap();
            std::fs::write(pet_dir.join(file), bytes).unwrap();
        }
        let local = pets::discover(&root);
        let forget = || {
            let _guard = STATE_LOCK.lock().unwrap();
            let mut state = State::load();
            state.0.remove(id);
            state.save();
        };
        forget();

        let status = |pets: &[PetInfo]| check(pets).remove(0).1;
        assert_eq!(status(&local), Status::UpToDate(Source::AwesomeCodexPet));
        {
            let _guard = STATE_LOCK.lock().unwrap();
            let mut state = State::load();
            if let Some(Known::Tracked { version, .. }) = state.0.get_mut(id) {
                *version = "antiga".into();
            }
            state.save();
        }
        assert_eq!(
            status(&local),
            Status::UpdateAvailable(Source::AwesomeCodexPet)
        );
        let found = updates(&local);
        apply(&pet_dir, &found[0]).unwrap();
        assert_eq!(status(&local), Status::UpToDate(Source::AwesomeCodexPet));

        forget();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn truncated_github_tree_is_an_error() {
        assert!(parse_awesome_tree(br#"{"truncated":true,"tree":[]}"#).is_err());
        assert!(parse_awesome_tree(br#"{"truncated":false,"tree":[]}"#).is_ok());
    }

    #[test]
    fn slug_check_blocks_path_tricks() {
        assert!(is_slug("chibi-naruto"));
        assert!(!is_slug("../x"));
        assert!(!is_slug("Goku"));
        assert!(!is_slug(""));
    }
}
