//! 核心层错误类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("配置解析错误: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("壳层错误: {0}")]
    Shell(String),
    #[error("渲染错误: {0}")]
    Render(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
