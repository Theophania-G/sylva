// 仅 Windows：把 sylva.ico 编译进 exe 资源。
// 资源 ID 1 = 应用主图标（Explorer / 文件属性 / Alt+Tab / 任务栏默认都读它）。
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    // 图标收在仓库顶层 `assets/`（crates/app 的上一级两级 = 仓库根）。
    let icon = std::path::Path::new(&manifest)
        .join("..")
        .join("..")
        .join("assets")
        .join("sylva.ico");
    let mut res = winres::WindowsResource::new();
    // 路径交给 rc.exe 时按字面使用：直接给绝对路径，避免 cwd 歧义
    res.set_icon(&icon.to_string_lossy());
    match res.compile() {
        Ok(()) => {}
        Err(e) => {
            // 找不到 rc.exe 等工具链问题时给出明确提示，不静默失败
            eprintln!(
                "[build.rs] winres 嵌入图标失败（{}）——检查 MSVC/Windows SDK 资源编译器",
                e
            );
            std::process::exit(1);
        }
    }
}
