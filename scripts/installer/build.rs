// 仅 Windows：把 sylva.ico 编译进安装器 exe 资源。
// 资源 ID 1 = 主图标（Explorer / 文件属性 / 安装窗口标题栏 / 任务栏都读它）。
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    // 图标收在仓库顶层 `assets/`（scripts/installer 的上级两级 = 仓库根）。
    let icon = std::path::Path::new(&manifest)
        .join("..")
        .join("..")
        .join("assets")
        .join("sylva.ico");
    let mut res = winres::WindowsResource::new();
    res.set_icon(&icon.to_string_lossy());
    match res.compile() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("[build.rs] winres 嵌入图标失败（{e}）——检查 MSVC/Windows SDK 资源编译器");
            std::process::exit(1);
        }
    }
}
