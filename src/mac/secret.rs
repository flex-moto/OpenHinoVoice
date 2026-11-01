//! Windows の DPAPI の代わり。鍵はキーチェーンに置き、データは AES-256-GCM で暗号化する
//! キーチェーンの鍵は、この Mac のこのユーザーでログインしているときだけ取り出せる

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use security_framework::passwords::{get_generic_password, set_generic_password};
use std::sync::OnceLock;

const SERVICE: &str = "OpenHinoVoice";
const ACCOUNT: &str = "storage-key";
const MAGIC: &[u8; 4] = b"OHV1";
const NONCE_LEN: usize = 12;
// security-framework が返す errSecItemNotFound
const ITEM_NOT_FOUND: i32 = -25300;

fn key() -> Result<[u8; 32], String> {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    if let Some(k) = KEY.get() {
        return Ok(*k);
    }
    let k = match get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => bytes
            .as_slice()
            .try_into()
            .map_err(|_| "キーチェーンの鍵の形式が不正です".to_string())?,
        Err(e) if e.code() == ITEM_NOT_FOUND => {
            let mut fresh = [0u8; 32];
            getrandom::fill(&mut fresh).map_err(|e| format!("乱数を作れません: {e}"))?;
            set_generic_password(SERVICE, ACCOUNT, &fresh)
                .map_err(|e| format!("キーチェーンに鍵を保存できません: {e}"))?;
            fresh
        }
        Err(e) => return Err(format!("キーチェーンから鍵を読めません: {e}")),
    };
    Ok(*KEY.get_or_init(|| k))
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
