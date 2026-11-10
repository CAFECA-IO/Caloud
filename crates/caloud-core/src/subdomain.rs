//! 平臺子網域 ID 規則（FR-ENV-07，ADR 0010）。
//!
//! ID 只含 `a-z0-9`、以字母開頭、同時含字母與數字；長度即環境種類。
//! 產生在 `caloud_auth::new_subdomain_id`，是否已被使用或封鎖由服務層查詢。
use serde::{Deserialize, Serialize};

/// ID 可用字元
pub const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
/// 長度至少這麼長的保留字，連「包含」它的 ID 也排除；更短的只排除完全相同
pub const RESERVED_SUBSTRING_MIN: usize = 3;
/// 後台可設定的 ID 長度範圍
pub const LEN_RANGE: std::ops::RangeInclusive<usize> = 6..=32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvKind {
    Production,
    Staging,
    Verify,
}

impl EnvKind {
    pub const ALL: [EnvKind; 3] = [EnvKind::Production, EnvKind::Staging, EnvKind::Verify];

    pub fn as_str(self) -> &'static str {
        match self {
            EnvKind::Production => "production",
            EnvKind::Staging => "staging",
            EnvKind::Verify => "verify",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// 各環境的 ID 長度（預設 8／12／16）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdLengths {
    pub production: usize,
    pub staging: usize,
    pub verify: usize,
}

impl Default for IdLengths {
    fn default() -> Self {
        Self {
            production: 8,
            staging: 12,
            verify: 16,
        }
    }
}

impl IdLengths {
    pub fn get(&self, kind: EnvKind) -> usize {
        match kind {
            EnvKind::Production => self.production,
            EnvKind::Staging => self.staging,
            EnvKind::Verify => self.verify,
        }
    }

    /// 長度須在範圍內且彼此不同（長度就是環境種類）
    pub fn check(&self) -> Result<(), String> {
        for k in EnvKind::ALL {
            if !LEN_RANGE.contains(&self.get(k)) {
                return Err(format!(
                    "{} 的 ID 長度需介於 {} 到 {}。",
                    k.as_str(),
                    LEN_RANGE.start(),
                    LEN_RANGE.end()
                ));
            }
        }
        if self.production == self.staging || self.staging == self.verify || self.production == self.verify {
            return Err("三種環境的 ID 長度必須不同，長度用來辨識環境種類。".into());
        }
        Ok(())
    }

    /// 由長度反推環境種類
    pub fn kind_of(&self, id: &str) -> Option<EnvKind> {
        EnvKind::ALL.into_iter().find(|k| self.get(*k) == id.len())
    }
}

#[derive(Debug, Clone, Default)]
pub struct IdRules {
    pub lengths: IdLengths,
    /// 保留字（小寫）
    pub reserved: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("長度應為 {expected}，實際為 {got}")]
    Length { expected: usize, got: usize },
    #[error("只能使用小寫英文字母與數字")]
    Charset,
    #[error("必須以英文字母開頭")]
    LeadingDigit,
    #[error("必須同時包含英文字母與數字")]
    NotMixed,
    #[error("包含保留字 `{0}`")]
    Reserved(String),
}

/// FR-ENV-07：檢查 ID 是否符合規則（不含「是否已被使用」）
pub fn validate_id(id: &str, kind: EnvKind, rules: &IdRules) -> Result<(), IdError> {
    let expected = rules.lengths.get(kind);
    if id.len() != expected {
        return Err(IdError::Length { expected, got: id.len() });
    }
    if !id.bytes().all(|b| ALPHABET.contains(&b)) {
        return Err(IdError::Charset);
    }
    if !id.as_bytes()[0].is_ascii_lowercase() {
        return Err(IdError::LeadingDigit);
    }
    let has_digit = id.bytes().any(|b| b.is_ascii_digit());
    let has_alpha = id.bytes().any(|b| b.is_ascii_lowercase());
    if !(has_digit && has_alpha) {
        return Err(IdError::NotMixed);
    }
    for word in &rules.reserved {
        let hit = if word.len() >= RESERVED_SUBSTRING_MIN {
            id.contains(word.as_str())
        } else {
            id == word
        };
        if hit {
            return Err(IdError::Reserved(word.clone()));
        }
    }
    Ok(())
}

/// 保留字格式：1–32 字元，`a-z0-9-`
pub fn valid_reserved_name(name: &str) -> bool {
    (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> IdRules {
        IdRules {
            lengths: IdLengths::default(),
            reserved: vec!["app".into(), "login".into(), "ww".into()],
        }
    }

    // FR-ENV-07
    #[test]
    fn validates_shape_and_reserved_words() {
        let r = rules();
        assert_eq!(validate_id("jo4f8rk4", EnvKind::Production, &r), Ok(()));
        assert_eq!(validate_id("k2m9xq7a4tz1", EnvKind::Staging, &r), Ok(()));
        assert_eq!(validate_id("r7b2n0w5c8e1h3pq", EnvKind::Verify, &r), Ok(()));
        assert!(matches!(
            validate_id("jo4f8rk", EnvKind::Production, &r),
            Err(IdError::Length { .. })
        ));
        assert_eq!(validate_id("Jo4f8rk4", EnvKind::Production, &r), Err(IdError::Charset));
        assert_eq!(validate_id("jo4f-rk4", EnvKind::Production, &r), Err(IdError::Charset));
        assert_eq!(validate_id("4of8rkab", EnvKind::Production, &r), Err(IdError::LeadingDigit));
        assert_eq!(validate_id("abcdefgh", EnvKind::Production, &r), Err(IdError::NotMixed));
        assert_eq!(
            validate_id("x1app9zz", EnvKind::Production, &r),
            Err(IdError::Reserved("app".into()))
        );
        // 短保留字只排除完全相同，不排除包含
        assert_eq!(validate_id("ww12ab34", EnvKind::Production, &r), Ok(()));
    }

    // FR-ENV-07、FR-OPS-03
    #[test]
    fn lengths_must_be_in_range_and_distinct() {
        assert!(IdLengths::default().check().is_ok());
        let same = IdLengths {
            production: 8,
            staging: 8,
            verify: 16,
        };
        assert!(same.check().is_err());
        let short = IdLengths {
            production: 4,
            staging: 12,
            verify: 16,
        };
        assert!(short.check().is_err());
        assert_eq!(IdLengths::default().kind_of("k2m9xq7a4tz1"), Some(EnvKind::Staging));
        assert_eq!(IdLengths::default().kind_of("abc"), None);
        assert!(valid_reserved_name("support-1"));
        assert!(!valid_reserved_name("Support"));
        assert!(!valid_reserved_name(""));
    }
}
