//! 祕密加密保存（FR-OPS-14，ADR 0016）。
//!
//! - 主金鑰 `CALOUD_MASTER_KEY`：32 bytes 的 base64，只從環境變數讀取
//! - ChaCha20-Poly1305，nonce 每次隨機；格式 `v1:<base64(nonce‖密文)>`
//! - AAD 為「用途:主鍵」，密文搬到別的列無法解開
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};

const PREFIX: &str = "v1:";

/// 主金鑰；Debug 不輸出內容
pub struct MasterKey(LessSafeKey);

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(..)")
    }
}

impl MasterKey {
    /// 讀取 `CALOUD_MASTER_KEY`；未設定回 `Ok(None)`，格式錯誤回錯誤
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        match std::env::var("CALOUD_MASTER_KEY") {
            Ok(v) if !v.trim().is_empty() => Self::from_base64(&v).map(Some),
            _ => Ok(None),
        }
    }

    pub fn from_base64(s: &str) -> anyhow::Result<Self> {
        let raw = B64
            .decode(s.trim())
            .map_err(|_| anyhow::anyhow!("CALOUD_MASTER_KEY 不是有效的 base64"))?;
        Self::from_bytes(&raw)
    }

    pub fn from_bytes(raw: &[u8]) -> anyhow::Result<Self> {
        if raw.len() != 32 {
            anyhow::bail!("CALOUD_MASTER_KEY 必須是 32 bytes（目前 {} bytes）", raw.len());
        }
        let key = UnboundKey::new(&CHACHA20_POLY1305, raw).map_err(|_| anyhow::anyhow!("主金鑰無效"))?;
        Ok(Self(LessSafeKey::new(key)))
    }

    /// 加密；`purpose` 與 `id` 組成 AAD
    pub fn seal(&self, purpose: &str, id: &str, plain: &[u8]) -> anyhow::Result<String> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new().fill(&mut nonce).map_err(|_| anyhow::anyhow!("無法產生亂數"))?;
        let mut buf = plain.to_vec();
        self.0
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), aad(purpose, id), &mut buf)
            .map_err(|_| anyhow::anyhow!("加密失敗"))?;
        let mut out = nonce.to_vec();
        out.extend(buf);
        Ok(format!("{PREFIX}{}", B64.encode(out)))
    }

    /// 解密；金鑰、用途或主鍵不符時失敗
    pub fn open(&self, purpose: &str, id: &str, sealed: &str) -> anyhow::Result<Vec<u8>> {
        let body = sealed.strip_prefix(PREFIX).ok_or_else(|| anyhow::anyhow!("不支援的密文格式"))?;
        let raw = B64.decode(body).map_err(|_| anyhow::anyhow!("密文不是有效的 base64"))?;
        if raw.len() < NONCE_LEN {
            anyhow::bail!("密文太短");
        }
        let (nonce, ct) = raw.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| anyhow::anyhow!("nonce 無效"))?;
        let mut buf = ct.to_vec();
        let plain = self
            .0
            .open_in_place(nonce, aad(purpose, id), &mut buf)
            .map_err(|_| anyhow::anyhow!("無法解密：主金鑰或資料不符"))?;
        Ok(plain.to_vec())
    }
}

fn aad(purpose: &str, id: &str) -> Aad<Vec<u8>> {
    Aad::from(format!("{purpose}:{id}").into_bytes())
}

/// 祕密的辨識提示：末 4 碼
pub fn hint(secret: &str) -> String {
    let tail: String = secret.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> MasterKey {
        MasterKey::from_bytes(&[7u8; 32]).unwrap()
    }

    // FR-OPS-14
    #[test]
    fn seals_and_opens() {
        let k = key();
        let s = k.seal("cloudflare_token", "caloud.tw", b"secret-token").unwrap();
        assert!(s.starts_with("v1:"));
        assert!(!s.contains("secret-token"));
        assert_eq!(k.open("cloudflare_token", "caloud.tw", &s).unwrap(), b"secret-token");
        // nonce 隨機：同一明文兩次結果不同
        assert_ne!(s, k.seal("cloudflare_token", "caloud.tw", b"secret-token").unwrap());
    }

    // FR-OPS-14：AAD 綁定用途與主鍵；換金鑰無法解開
    #[test]
    fn rejects_wrong_context_or_key() {
        let s = key().seal("cloudflare_token", "caloud.tw", b"x").unwrap();
        assert!(key().open("cloudflare_token", "other.tw", &s).is_err());
        assert!(key().open("env", "caloud.tw", &s).is_err());
        assert!(MasterKey::from_bytes(&[8u8; 32])
            .unwrap()
            .open("cloudflare_token", "caloud.tw", &s)
            .is_err());
        assert!(key().open("cloudflare_token", "caloud.tw", "v0:abc").is_err());
    }

    #[test]
    fn key_length_and_hint() {
        assert!(MasterKey::from_bytes(&[0u8; 16]).is_err());
        assert!(MasterKey::from_base64("not base64!").is_err());
        assert!(MasterKey::from_base64(&B64.encode([1u8; 32])).is_ok());
        assert_eq!(hint("abcdefgh"), "…efgh");
        assert_eq!(hint("ab"), "…ab");
        assert!(format!("{:?}", key()).contains(".."));
    }
}
