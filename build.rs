fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winres::WindowsResource::new();

        res.set_icon("app_icon.ico");

        // Release installs must elevate. Other profiles stay asInvoker so
        // cargo test can launch the harness without error 740.
        let level = if std::env::var("PROFILE").unwrap_or_default() == "release" {
            "requireAdministrator"
        } else {
            "asInvoker"
        };

        res.set_manifest(&format!(
            r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="{level}" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#
        ));

        res.compile()
            .unwrap_or_else(|e| panic!("Failed to compile Windows resource: {e}"));
    }
}
