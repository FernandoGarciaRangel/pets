//! HTTPS via WinHTTP e hashes via BCrypt, ambos do Windows, para não puxar crates de rede/TLS.

use std::ptr::{null, null_mut};

const MAX_DOWNLOAD: usize = 32 * 1024 * 1024;
/// Prazo do download inteiro. Os timeouts do WinHTTP valem por operação: um servidor que manda
/// um pouco a cada 29 s seguraria a thread (e o "atualizando…" do menu) para sempre.
const DOWNLOAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5 * 60);

pub struct Response {
    pub status: u32,
    pub body: Vec<u8>,
    pub content_length: Option<u64>,
}

/// GET que exige 200.
pub fn get(url: &str) -> Result<Vec<u8>, String> {
    let response = request("GET", url)?;
    if response.status != 200 {
        return Err(format!("HTTP {} em {url}", response.status));
    }
    Ok(response.body)
}

/// Requisição HTTPS; segue redirects https→https (GitHub e petdex usam para os arquivos).
/// Em `HEAD` o corpo vem vazio e só `content_length` interessa.
pub fn request(method: &str, url: &str) -> Result<Response, String> {
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

    let agent = wide(&format!("pets/{}", env!("CARGO_PKG_VERSION")));
    let (host_w, path_w, verb) = (wide(host), wide(&path), wide(method));
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
        let mut length_buf = [0u16; 32];
        let mut length_len = (length_buf.len() * 2) as u32;
        let content_length = (WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_CONTENT_LENGTH,
            null(),
            length_buf.as_mut_ptr().cast(),
            &mut length_len,
            null_mut(),
        ) != 0)
            .then(|| String::from_utf16_lossy(&length_buf[..length_len as usize / 2]))
            .and_then(|s| s.trim().parse().ok());

        let mut body = Vec::new();
        if method != "HEAD" {
            if content_length.is_some_and(|n| n > MAX_DOWNLOAD as u64) {
                return Err(format!("download maior que o limite em {url}"));
            }
            let deadline = std::time::Instant::now() + DOWNLOAD_DEADLINE;
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                if std::time::Instant::now() > deadline {
                    return Err(format!("download lento demais em {url}"));
                }
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
            // Conexão que cai no meio termina como fim normal do corpo; só o tamanho denuncia.
            if let Some(expected) = content_length
                && body.len() as u64 != expected
            {
                return Err(format!(
                    "download incompleto em {url} ({} de {expected} bytes)",
                    body.len()
                ));
            }
        }
        Ok(Response {
            status,
            body,
            content_length,
        })
    }
}

fn hash_hex(
    algorithm: windows_sys::Win32::Security::Cryptography::BCRYPT_ALG_HANDLE,
    data: &[u8],
    out_len: usize,
) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::BCryptHash;
    let mut out = vec![0u8; out_len];
    // SAFETY: buffers válidos com os tamanhos informados.
    let status = unsafe {
        BCryptHash(
            algorithm,
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

pub fn sha256_hex(data: &[u8]) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::BCRYPT_SHA256_ALG_HANDLE;
    hash_hex(BCRYPT_SHA256_ALG_HANDLE, data, 32)
}

/// O hash que o git (e a API de árvores do GitHub) dá a um arquivo: SHA-1 de `blob <len>\0<dados>`.
pub fn git_blob_sha1(data: &[u8]) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::BCRYPT_SHA1_ALG_HANDLE;
    let mut blob = format!("blob {}\0", data.len()).into_bytes();
    blob.extend_from_slice(data);
    hash_hex(BCRYPT_SHA1_ALG_HANDLE, &blob, 20)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn git_blob_sha1_matches_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            git_blob_sha1(b"hello\n").unwrap(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }
}
