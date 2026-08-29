// 仅 Windows：把 sylva.ico 编译进安装器 exe 资源，并内嵌 DPI 感知清单。
// 资源 ID 1 = 主图标（Explorer / 文件属性 / 安装窗口标题栏 / 任务栏都读它）。
//
// 关键：winres 默认**不**内嵌任何清单——没有 DPI 感知清单时，进程被 Windows 判定为
// DPI 非感知 → 高缩放下整窗按 96 DPI 渲染再被拉伸，即「清晰度很低、文字没对正」的根因。
// 这里显式嵌入 Per-Monitor v2 清单（含 Common-Controls v6 依赖，进度条/按钮走现代主题）。
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
    res.set("ProductName", "Sylva 桌面栅栏整理器");
    res.set("FileDescription", "Sylva 安装程序");
    res.set_manifest(MANIFEST);
    match res.compile() {
        Ok(()) => {}
        Err(e) => {
            eprintln!(
                "[build.rs] winres 嵌入图标/清单失败（{e}）——检查 MSVC/Windows SDK 资源编译器"
            );
            std::process::exit(1);
        }
    }
}

/// Per-Monitor v2 感知清单：高缩放下原生渲染（清晰），跨显示器 DPI 变化实时重排。
/// `true/pm`（SMI/2005）供 Win8.1 回退，`PerMonitorV2`（SMI/2016）为 Win10 1703+ 主路径。
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Sylva.Setup" version="0.1.0.0" processorArchitecture="*"/>
  <description>Sylva Desktop Fences Setup</description>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
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
