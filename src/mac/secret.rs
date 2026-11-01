//! Windows の DPAPI の代わり。データは AES-256-GCM で暗号化し、鍵は設定フォルダの storage.key に置く
//! 鍵のファイルはこのユーザーだけが読める(600)。DPAPI と同じく、ほかのユーザーからは読めず、
//! このユーザーで動くプログラムからは読める。キーチェーンに置くと、アプリを作り直したり更新したりするたびに
//! 「キーチェーンの情報を使おうとしています」と聞かれるため使わない

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use std::sync::OnceLock;

const MAGIC: &[u8; 4] = b"OHV1";
const NONCE_LEN: usize = 12;

/// テストでは本物の鍵のファイルに触らない
#[cfg(test)]
fn key() -> Result<[u8; 32], String> {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    Ok(*KEY.get_or_init(|| {
        let mut k = [0u8; 32];
        getrandom::fill(&mut k).expect("system random number generator failed");
        k
    }))
}

#[cfg(not(test))]
fn key() -> Result<[u8; 32], String> {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    if let Some(k) = KEY.get() {
        return Ok(*k);
    }
    let k = load_or_create_key(&crate::mac::app_dir().join("storage.key"))?;
    Ok(*KEY.get_or_init(|| k))
}

/// 常駐側と設定画面が同時に作ろうとしても、先に作られた方を使う
#[cfg_attr(test, allow(dead_code))]
fn load_or_create_key(path: &std::path::Path) -> Result<[u8; 32], String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let read = |path: &std::path::Path| -> Result<[u8; 32], String> {
        std::fs::read(path)
            .map_err(|e| format!("暗号化の鍵を読めません: {e}"))?
            .as_slice()
            .try_into()
            .map_err(|_| "暗号化の鍵の形式が不正です".to_string())
    };
    if path.exists() {
        return read(path);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("設定フォルダを作れません: {e}"))?;
    }
    let mut fresh = [0u8; 32];
    getrandom::fill(&mut fresh).map_err(|e| format!("乱数を作れません: {e}"))?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(&fresh)
                .and_then(|_| file.sync_all())
                .map_err(|e| format!("暗号化の鍵を保存できません: {e}"))?;
            Ok(fresh)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read(path),
        Err(e) => Err(format!("暗号化の鍵を保存できません: {e}")),
    }
}

/// 今の macOS ユーザーだけが復号できる形に暗号化する
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(&key()?.into());
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| format!("乱数を作れません: {e}"))?;
    let sealed = cipher
        .encrypt(Nonce::from_slice(&nonce), data)
        .map_err(|_| "データを暗号化できません".to_string())?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + sealed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    let body = data
        .strip_prefix(MAGIC)
        .filter(|b| b.len() > NONCE_LEN)
        .ok_or("暗号化データの形式が不正です")?;
    let (nonce, sealed) = body.split_at(NONCE_LEN);
    Aes256Gcm::new(&key()?.into())
        .decrypt(Nonce::from_slice(nonce), sealed)
        .map_err(|_| "データを復号できません".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_data_round_trips_and_detects_tampering() {
        let sealed = protect(b"secret").unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert_eq!(unprotect(&sealed).unwrap(), b"secret");
        let mut broken = sealed.clone();
        *broken.last_mut().unwrap() ^= 1;
        assert!(unprotect(&broken).is_err());
        assert!(unprotect(b"OHV1").is_err());
    }

    #[test]
    fn key_file_is_private_and_reused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ohv-key-{}", std::process::id()));
        let path = dir.join("storage.key");
        let first = load_or_create_key(&path).unwrap();
        assert_eq!(load_or_create_key(&path).unwrap(), first);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
