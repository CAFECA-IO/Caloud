//! Sign in with CAFECA — Rust 伺服器端驗證。
//!
//! 依 CAFECA 官方規格（README 第 4 節）實作：
//! 1. 檢查 domain / nonce / chainId / expiresAt
//! 2. 計算 EIP-712 `SignIn` hash（domain = `CAFECA Sign-In` v1, verifyingContract = account）
//! 3. `eth_call` 呼叫 `account.isValidSignature(hash, signature)`，需回傳 `0x1626ba7e`
//!
//! 驗證完全在本機完成，只依賴公開 RPC，不需要 CAFECA API key。
#![forbid(unsafe_code)]

use alloy_primitives::{Address, Bytes, FixedBytes, U256};
use alloy_sol_types::{sol, Eip712Domain, SolCall, SolStruct};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use tokio::sync::OnceCell;

sol! {
    struct SignIn {
        string domain;
        string uri;
        string nonce;
        uint256 issuedAt;
        uint256 expiresAt;
        string statement;
        string claims;
        string channel;
    }

    function isValidSignature(bytes32 hash, bytes signature) external view returns (bytes4);
}

const ERC1271_MAGIC: [u8; 4] = [0x16, 0x26, 0xba, 0x7e];

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("使用者拒絕或錢包回傳錯誤：{0}")]
    Wallet(String),
    #[error("網域不符")]
    DomainMismatch,
    #[error("nonce 不符")]
    NonceMismatch,
    #[error("chainId 不符")]
    ChainMismatch,
    #[error("登入請求已過期")]
    Expired,
    #[error("簽章無效")]
    BadSignature,
    #[error("格式錯誤：{0}")]
    Malformed(String),
    #[error("RPC 錯誤：{0}")]
    Rpc(String),
}

/// `<wallet>/.well-known/cafeca-configuration`
#[derive(Debug, Clone, Deserialize)]
pub struct CafecaConfiguration {
    pub issuer: String,
    pub chain: ChainInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChainInfo {
    pub id: u64,
    pub rpc: String,
}

/// 錢包交回的登入回應（前端原封不動 POST 給後端）
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SignInResponse {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub error: Option<String>,
    pub account: Option<Address>,
    #[serde(rename = "chainId")]
    pub chain_id: Option<u64>,
    pub message: Option<SignInMessage>,
    pub signature: Option<Bytes>,
    #[serde(default)]
    pub claims: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInMessage {
    pub domain: String,
    pub uri: String,
    pub nonce: String,
    pub issued_at: u64,
    pub expires_at: u64,
    #[serde(default)]
    pub statement: String,
    #[serde(default)]
    pub claims: String,
    #[serde(default)]
    pub channel: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifiedSignIn {
    /// 身分合約地址（小寫 0x…），即使用者唯一 ID
    pub account: String,
    /// 由錢包查詢確認過的代稱
    pub handle: Option<String>,
}

pub struct CafecaVerifier {
    wallet: String,
    rpc_override: Option<String>,
    http: reqwest::Client,
    config: OnceCell<CafecaConfiguration>,
}

impl CafecaVerifier {
    pub fn new(wallet: impl Into<String>, rpc_override: Option<String>) -> Self {
        Self {
            wallet: wallet.into().trim_end_matches('/').to_string(),
            rpc_override,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("http client"),
            config: OnceCell::new(),
        }
    }

    pub fn wallet(&self) -> &str {
        &self.wallet
    }

    pub async fn config(&self) -> anyhow::Result<&CafecaConfiguration> {
        self.config
            .get_or_try_init(|| async {
                let url = format!("{}/.well-known/cafeca-configuration", self.wallet);
                let cfg = self.http.get(url).send().await?.error_for_status()?.json().await?;
                anyhow::Ok(cfg)
            })
            .await
    }

    /// 驗證登入回應。`domain` 為本站 origin，`nonce` 為本站發出且已從儲存中**取出作廢**的 nonce。
    pub async fn verify(&self, res: &SignInResponse, domain: &str, nonce: &str, now: u64) -> Result<VerifiedSignIn, AuthError> {
        if let Some(e) = &res.error {
            return Err(AuthError::Wallet(e.clone()));
        }
        if res.v != 1 || res.kind != "cafeca:auth" {
            return Err(AuthError::Malformed("v/type".into()));
        }
        let account = res.account.ok_or_else(|| AuthError::Malformed("account".into()))?;
        let msg = res.message.as_ref().ok_or_else(|| AuthError::Malformed("message".into()))?;
        let sig = res.signature.clone().ok_or_else(|| AuthError::Malformed("signature".into()))?;
        let cfg = self.config().await.map_err(|e| AuthError::Rpc(e.to_string()))?;

        if msg.domain != domain {
            return Err(AuthError::DomainMismatch);
        }
        if msg.nonce != nonce {
            return Err(AuthError::NonceMismatch);
        }
        if res.chain_id != Some(cfg.chain.id) {
            return Err(AuthError::ChainMismatch);
        }
        if now >= msg.expires_at || msg.issued_at > now + 60 {
            return Err(AuthError::Expired);
        }

        let hash = signin_hash(msg, cfg.chain.id, account);
        let rpc = self.rpc_override.as_deref().unwrap_or(&cfg.chain.rpc);
        if !self.is_valid_signature(rpc, account, hash, sig).await? {
            return Err(AuthError::BadSignature);
        }

        let handle = if msg.claims.split(',').any(|c| c == "handle") {
            self.lookup_handle(account).await
        } else {
            None
        };
        Ok(VerifiedSignIn {
            account: format!("{account:#x}"),
            handle,
        })
    }

    async fn is_valid_signature(&self, rpc: &str, account: Address, hash: FixedBytes<32>, sig: Bytes) -> Result<bool, AuthError> {
        let data = isValidSignatureCall { hash, signature: sig }.abi_encode();
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "eth_call",
            "params": [{ "to": account, "data": format!("0x{}", hex::encode(data)) }, "latest"],
        });
        let v: serde_json::Value = self
            .http
            .post(rpc)
            .json(&body)
            .send()
            .await
            .map_err(|e| AuthError::Rpc(e.to_string()))?
            .json()
            .await
            .map_err(|e| AuthError::Rpc(e.to_string()))?;
        // revert（例如帳戶尚未部署或簽章格式錯）視為無效簽章
        let Some(out) = v.get("result").and_then(|r| r.as_str()) else {
            return Ok(false);
        };
        let out = hex::decode(out.trim_start_matches("0x")).unwrap_or_default();
        Ok(out.len() >= 4 && out[..4] == ERC1271_MAGIC)
    }

    async fn lookup_handle(&self, account: Address) -> Option<String> {
        #[derive(Deserialize)]
        struct P {
            handle: Option<String>,
        }
        let url = format!("{}/api/profile?q={account:#x}", self.wallet);
        let p: P = self.http.get(url).send().await.ok()?.json().await.ok()?;
        p.handle
            .filter(|h| h.len() >= 3 && h.len() <= 20 && h.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    }
}

/// EIP-712 `hashTypedData({ name: "CAFECA Sign-In", version: "1", chainId, verifyingContract: account }, SignIn, msg)`
pub fn signin_hash(msg: &SignInMessage, chain_id: u64, account: Address) -> FixedBytes<32> {
    let domain = Eip712Domain::new(
        Some(Cow::Borrowed("CAFECA Sign-In")),
        Some(Cow::Borrowed("1")),
        Some(U256::from(chain_id)),
        Some(account),
        None,
    );
    SignIn {
        domain: msg.domain.clone(),
        uri: msg.uri.clone(),
        nonce: msg.nonce.clone(),
        issuedAt: U256::from(msg.issued_at),
        expiresAt: U256::from(msg.expires_at),
        statement: msg.statement.clone(),
        claims: msg.claims.clone(),
        channel: msg.channel.clone(),
    }
    .eip712_signing_hash(&domain)
}

/// 128-bit 隨機 nonce，base64url 無補齊（22 字元，符合 `[A-Za-z0-9_-]{8,128}`）
pub fn new_nonce() -> String {
    let b: [u8; 16] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// 256-bit session token
pub fn new_token() -> String {
    let b: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-AUTH-02
    #[test]
    fn nonce_format() {
        let n = new_nonce();
        assert!(n.len() >= 8 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }

    // FR-AUTH-01
    #[test]
    fn hash_is_deterministic_and_domain_bound() {
        let m = SignInMessage {
            domain: "https://caloud.example".into(),
            uri: "https://caloud.example/".into(),
            nonce: "abcdefgh".into(),
            issued_at: 1,
            expires_at: 2,
            statement: String::new(),
            claims: "handle".into(),
            channel: String::new(),
        };
        let a = Address::repeat_byte(0x11);
        assert_eq!(signin_hash(&m, 8018, a), signin_hash(&m, 8018, a));
        let mut other = m.clone();
        other.domain = "https://evil.example".into();
        assert_ne!(signin_hash(&m, 8018, a), signin_hash(&other, 8018, a));
        assert_ne!(signin_hash(&m, 8018, a), signin_hash(&m, 1, a));
        // 與 viem `hashTypedData`（CAFECA 官方 SDK 所用）交叉比對的測試向量
        assert_eq!(
            format!("{:#x}", signin_hash(&m, 8018, a)),
            "0xe553c2ed057012dc24ac0ad9a029b0ff85fe18f21d0779494105fc9eb1b5f8b4"
        );
    }
}
