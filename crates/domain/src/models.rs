use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SiteId(String);

impl SiteId {
    pub fn new(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        // SiteId 规则：仅限小写字母、数字、点、横杠，且不包含下划线
        // 下划线保留作为 Adapter 中 {site_id}_{slug} 的分隔符
        if s.contains('_') {
            return Err("Site ID cannot contain underscores ('_'). Please use hyphens ('-') or dots ('.') instead.".to_string());
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        {
            return Err("Site ID contains invalid characters.".to_string());
        }
        if s.len() > 64 {
            return Err("Site ID is too long (max 64 chars).".to_string());
        }
        Ok(Self(s))
    }

    /// 仅供数据库读取信任数据时使用，跳过校验
    pub fn new_unchecked(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String, // Matrix Event ID
    pub site_id: SiteId,
    pub post_slug: String,

    // 作者信息
    pub author_id: String,        // Matrix User ID
    pub author_name: String,      // Display Name
    pub author_fingerprint: Option<String>, // 仅 Guest 有
    pub avatar_url: Option<String>, // 新增：原生头像 (mxc://) 或 代理链接
    pub is_guest: bool,

    // 内容与状态
    pub content: String,
    pub is_redacted: bool,

    // 结构关系
    pub reply_to: Option<String>,

    // 时间
    pub created_at: NaiveDateTime,
    pub updated_at: Option<NaiveDateTime>,

    // 乐观 UI 支持
    pub txn_id: Option<String>, // 新增：前端生成的唯一 ID
}
