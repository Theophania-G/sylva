// 仅 Windows：把 sylva.ico 编译进 exe 资源，并内嵌 DPI 感知清单。
// 资源 ID 1 = 主图标（Explorer / 文件属性 / Alt+Tab / 任务栏默认都读它）。
//
// 关键：winres 默认**不**内嵌任何清单——没有 DPI 感知清单时，进程被 Windows 判定为
// DPI 非感知/系统感知，高缩放下 DWM 把整窗位图缩放（整窗发糊 + 拖拽/动画时重采样
// 与合成竞争 → 闪烁）。这里显式嵌入 Per-Monitor v2 清单：窗口按真实物理像素渲染，
// 跨显示器 DPI 变化由 `WM_DPICHANGED` 实时重排（与 scripts/installer 做法对齐）。
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
    res.set_manifest(MANIFEST);
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

/// Per-Monitor v2 感知清单：高缩放下原生渲染（清晰），跨显示器 DPI 变化实时重排。
/// `true/pm`（SMI/2005）供 Win8.1 回退，`PerMonitorV2`（SMI/2016）为 Win10 1703+ 主路径。
/// 与 scripts/installer 的清单一致，仅 assemblyIdentity 名称不同。
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Sylva.App" version="0.1.0.0" processorArchitecture="*"/>
  <description>Sylva Desktop Fences</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
      <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>"#;
