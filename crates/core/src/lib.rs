//! `sylva-core`：桌面栅栏整理器的纯 Rust 业务内核。
//!
//! 本 crate **零 Win32 依赖**，只包含领域模型与纯算法，
//! 保证可以在任何环境单元测试。平台相关逻辑全部放在上层：
//!
//! ```text
//! sylva-core (纯模型/算法)  ←  sylva-shell (Windows 壳层)
//!                              sylva-render (DirectComposition/D2D)
//!                              fence-ui (egui 设置)
//!                                       ←  sylva-app (组合根)
//! ```

pub mod animation;
pub mod config;
pub mod details;
pub mod error;
pub mod event;
pub mod layout;
pub mod magnet;
pub mod model;

pub use error::{CoreError, Result};
