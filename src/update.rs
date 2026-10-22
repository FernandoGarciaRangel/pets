//! Atualização pelo GitHub Releases: checa a última release, baixa o `pets.exe`, confere o SHA-256
//! com o `SHA256SUMS.txt` da mesma release e entrega a instalação ao `pets.exe install` baixado.
//! HTTP via WinHTTP e hash via BCrypt, ambos do Windows, para não puxar crates de rede/TLS.

use std::path::PathBuf;
use std::ptr::{null, null_mut};

const REPO: &str = "FernandoGarciaRangel/pets";
const EXE_ASSET: &str = "pets.exe";
const SUMS_ASSET: &str = "SHA256SUMS.txt";
const MAX_DOWNLOAD: usize = 32 * 1024 * 1024;
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

fn sha256_hex(data: &[u8]) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    // SAFETY: buffers válidos com os tamanhos informados.
    let status = unsafe {
        BCryptHash(
            BCRYPT_SHA256_ALG_HANDLE,
            null(),
            0,
            data.as_ptr(),
            data.len() as u32,
            out.as_mut_ptr(),
            out.len() as u32,
        )
    };
    if status != 0 {
        return Err(format!("BCryptHash falhou: 0x{status:08x}"));
    }
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// GET HTTPS com WinHTTP (segue redirects https→https, que o GitHub usa para os assets).
fn http_get(url: &str) -> Result<Vec<u8>, String> {
    use crate::system::wide;
    use windows_sys::Win32::Networking::WinHttp::*;

    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("URL não é https: {url}"))?;
    let (host, path) = rest
        .split_once('/')
        .map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));

    struct Handle(*mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: handle WinHTTP aberto por nós.
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }
    let fail = |what: &str| {
        // SAFETY: sem pré-condições.
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        format!("{what} falhou (erro {code}) em {url}")
    };

    let agent = wide(&format!("pets/{}", current_version()));
    let (host_w, path_w, verb) = (wide(host), wide(&path), wide("GET"));
    // SAFETY: strings terminadas em NUL; handles fechados pelo Drop na ordem inversa.
    unsafe {
        let session = Handle(WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            null(),
            null(),
            0,
        ));
        if session.0.is_null() {
            return Err(fail("WinHttpOpen"));
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let connect = Handle(WinHttpConnect(
            session.0,
            host_w.as_ptr(),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if connect.0.is_null() {
            return Err(fail("WinHttpConnect"));
        }
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            path_w.as_ptr(),
            null(),
            null(),
            null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(fail("WinHttpOpenRequest"));
        }
        if WinHttpSendRequest(request.0, null(), 0, null(), 0, 0, 0) == 0 {
            return Err(fail("conexão"));
        }
        if WinHttpReceiveResponse(request.0, null_mut()) == 0 {
            return Err(fail("resposta"));
        }
        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            (&mut status as *mut u32).cast(),
            &mut len,
            null_mut(),
        );
        if status != 200 {
            return Err(format!("HTTP {status} em {url}"));
        }
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            if WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            ) == 0
            {
                return Err(fail("leitura"));
            }
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > MAX_DOWNLOAD {
                return Err(format!("download maior que o limite em {url}"));
            }
        }
        Ok(body)
    }
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

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
