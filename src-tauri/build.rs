fn main() {
    // 自定义 app.manifest：强制以管理员权限（requireAdministrator）启动，
    // 这样结束被系统/其他提权进程占用的文件时才不会被拒绝。
    // 如需改为普通权限启动，把 level 换成 "asInvoker" 并重新编译即可。
    const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 / 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" />
    </application>
  </compatibility>
</assembly>"#;

    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new().app_manifest(MANIFEST)),
    )
    .expect("failed to run tauri-build");
}
