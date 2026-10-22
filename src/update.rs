//! Atualização pelo GitHub Releases: checa a última release, baixa o `pets.exe`, confere o SHA-256
//! com o `SHA256SUMS.txt` da mesma release e entrega a instalação ao `pets.exe install` baixado.

use crate::net::{get as http_get, sha256_hex};
use std::path::PathBuf;

const REPO: &str = "FernandoGarciaRangel/pets";
const EXE_ASSET: &str = "pets.exe";
const SUMS_ASSET: &str = "SHA256SUMS.txt";
pub const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub exe_url: String,
    pub sums_url: String,
}

pub enum CheckResult {
    Available(Release),
    UpToDate,
    Failed(String),
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.split(['-', '+']).next()?.parse().ok();
    Some((next()?, next()?, next()?))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Lê o JSON de `releases/latest` da API do GitHub.
pub fn parse_latest(json: &[u8]) -> Result<Release, String> {
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| format!("resposta inválida do GitHub: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("release sem tag_name")?;
    let asset = |name: &str| {
        v["assets"].as_array().and_then(|assets| {
            assets
                .iter()
                .find(|a| a["name"] == name)
                .and_then(|a| a["browser_download_url"].as_str().map(str::to_string))
        })
    };
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        exe_url: asset(EXE_ASSET).ok_or("release sem pets.exe")?,
        sums_url: asset(SUMS_ASSET).ok_or("release sem SHA256SUMS.txt")?,
    })
}

/// Hash esperado de `name` num arquivo no formato do `sha256sum`.
pub fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.trim_start_matches('\u{feff}')
        .lines()
        .find_map(|line| {
            let (hash, file) = line.trim().split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
        })
}

pub fn check() -> CheckResult {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    match http_get(&url).and_then(|body| parse_latest(&body)) {
        Ok(release) if is_newer(&release.version, current_version()) => {
            CheckResult::Available(release)
        }
        Ok(_) => CheckResult::UpToDate,
        Err(e) => CheckResult::Failed(e),
    }
}

/// Baixa e confere o exe da release; devolve o caminho do arquivo pronto para `install`.
pub fn download(release: &Release) -> Result<PathBuf, String> {
    let sums = String::from_utf8_lossy(&http_get(&release.sums_url)?).into_owned();
    let expected = expected_hash(&sums, EXE_ASSET).ok_or("SHA256SUMS.txt sem o pets.exe")?;
    let exe = http_get(&release.exe_url)?;
    let actual = sha256_hex(&exe)?;
    if actual != expected {
        return Err(format!(
            "o SHA-256 do download não confere (esperado {expected}, veio {actual})"
        ));
    }
    let dir = staging_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(EXE_ASSET);
    std::fs::write(&path, exe).map_err(|e| format!("não consegui salvar o download: {e}"))?;
    Ok(path)
}

/// `pets.exe update`: o mesmo caminho do menu (checar, baixar, conferir, `install --from-update`).
pub fn update_now() -> Result<String, String> {
    match check() {
        CheckResult::Available(release) => {
            let path = download(&release)?;
            crate::install::spawn_detached(&path, &["install", "--from-update"])?;
            Ok(format!(
                "v{} baixada e conferida; instalando em segundo plano",
                release.version
            ))
        }
        CheckResult::UpToDate => Ok(format!(
            "já na versão mais recente (v{})",
            current_version()
        )),
        CheckResult::Failed(e) => Err(e),
    }
}

/// Pasta temporária do download; o app limpa ao iniciar.
pub fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("pets-update")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semver() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("lixo", "0.1.0"));
    }

    #[test]
    fn parses_github_latest_release() {
        let json = br#"{"tag_name":"v0.2.0","assets":[
            {"name":"pets-v0.2.0-windows-x64.zip","browser_download_url":"https://x/zip"},
            {"name":"pets.exe","browser_download_url":"https://x/pets.exe"},
            {"name":"SHA256SUMS.txt","browser_download_url":"https://x/sums"}]}"#;
        let r = parse_latest(json).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.exe_url, "https://x/pets.exe");
        assert_eq!(r.sums_url, "https://x/sums");
        assert!(parse_latest(br#"{"tag_name":"v1","assets":[]}"#).is_err());
    }

    #[test]
    fn finds_hash_in_sha256sums() {
        let sums = "\u{feff}AAA  pets-v0.1.0-windows-x64.zip\r\nBBB  pets.exe\r\n";
        assert_eq!(expected_hash(sums, "pets.exe").as_deref(), Some("bbb"));
        assert_eq!(expected_hash(sums, "outro.exe"), None);
    }
}
