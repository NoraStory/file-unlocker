fn main() {
    // 自定义 app.manifest：强制以管理员权限（requireAdministrator）启动，
    // 这样结束被系统/其他提权进程占用的文件时才不会被拒绝。
    // 如需改为普通权限启动，把 level 换成 "asInvoker" 并重新编译即可。
    const MANIFEST_ADMIN: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
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

    // 测试二进制不注入提权 manifest：cargo test 需要在普通终端 / CI 中运行。
    // 注意：tauri-build → embed-resource 会把资源（含 manifest）以
    // rustc-link-arg-bins 链接到本 crate 的 *bin* 测试 harness（无法绕过），
    // 因此运行测试请用 `cargo test --lib`（全部 12 个测试都在 lib 中，
    // main.rs 仅有 3 行启动代码，无可测逻辑）。
    const MANIFEST_TEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" />
    </application>
  </compatibility>
</assembly>"#;

    // PROFILE 由 cargo 传给 build script：dev/release 为主程序构建，test 为测试构建
    let manifest = match std::env::var("PROFILE").as_deref() {
        Ok("test") => MANIFEST_TEST,
        _ => MANIFEST_ADMIN,
    };

    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new().app_manifest(manifest)),
    )
    .expect("failed to run tauri-build");
}
