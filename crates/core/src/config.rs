//! 应用级设置与持久化。
//!
//! 数据目录由上层注入（Windows 上为 `%APPDATA%\Sylva`），
//! 核心层不感知平台路径，保持可测。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::model::Desk;

/// 全局设置，随 `Desk` 一起持久化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    /// 是否显示未分组图标区。
    pub show_free_area: bool,
    /// 未分组图标区高度（逻辑 px）。
    pub free_area_height: f32,
    pub hotkeys: HotkeyConfig,
    /// 是否开机自启。
    pub autostart: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            show_free_area: true,
            free_area_height: 90.0,
            hotkeys: HotkeyConfig::default(),
            autostart: false,
        }
    }
}

/// 全局热键配置。值为可解析的键位描述（如 `"Ctrl+Shift+F"`）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HotkeyConfig {
    pub collapse_all: Option<String>,
    pub expand_all: Option<String>,
    pub search: Option<String>,
}

/// 配置读写器：负责 `Desk` 的加载、保存与版本迁移。
pub struct ConfigStore {
    dir: PathBuf,
}

impl ConfigStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn desk_path(&self) -> PathBuf {
        self.dir.join("desk.json")
    }

    /// 加载桌面状态；文件不存在时返回默认值（不报错）。
    pub fn load(&self) -> Result<Desk, crate::error::CoreError> {
        let path = self.desk_path();
        if !path.exists() {
            return Ok(Desk::new(AppSettings::default()));
        }
        let raw = std::fs::read_to_string(&path)?;
        let mut desk: Desk = serde_json::from_str(&raw)?;
        migrate(&mut desk);
        desk.validate();
        Ok(desk)
    }

    /// 原子保存：先写临时文件再改名，避免崩溃造成配置损坏。
    pub fn save(&self, desk: &Desk) -> Result<(), crate::error::CoreError> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.desk_path();
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(desk)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// 明确删除配置文件（用于「重置所有设置」）。
    pub fn delete(&self) -> Result<(), crate::error::CoreError> {
        let path = self.desk_path();
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

/// 版本迁移：结构变化时在此按版本推进。
///
/// 规则：永远向前迁移；每次发布新结构时新增一段 `if desk.version < N`。
fn migrate(desk: &mut Desk) {
    // v1：初始版本，暂无迁移逻辑。
    let _ = desk;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Fence;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sylva-core-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_returns_default_when_missing() {
        let store = ConfigStore::new(tmp_dir("missing"));
        let desk = store.load().unwrap();
        assert!(desk.fences.is_empty());
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tmp_dir("roundtrip");
        let store = ConfigStore::new(dir.clone());
        let mut desk = Desk::new(AppSettings::default());
        desk.fences.push(Fence {
            id: 1,
            title: Some("工作".into()),
            monitor_id: 0,
            bounds: crate::model::Rect::new(0.0, 0.0, 300.0, 200.0),
            state: crate::model::FenceState::Expanded,
            icon_ids: Vec::new(),
            appearance: crate::model::FenceAppearance::default(),
            scroll: 0.0,
        });
        store.save(&desk).unwrap();

        let loaded = ConfigStore::new(dir).load().unwrap();
        assert_eq!(loaded, desk);
        assert_eq!(loaded.fences[0].title.as_deref(), Some("工作"));
    }
}
