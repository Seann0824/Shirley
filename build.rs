fn main() {
    // Tauri 2.7 only emits `rerun-if-changed` for `tauri.conf.json` / `capabilities`,
    // **not for `bundle.icon`**. A macOS dev build embeds the bytes of `icons/icon.icns`
    // directly into the binary and sets it as the Dock icon at startup -- so without this line,
    // after re-exporting the icon, `cargo build` is a no-op and the Dock stays on the old icon.
    println!("cargo:rerun-if-changed=icons");

    // Only when the `desktop` feature is enabled do we let Tauri generate the context / resource bindings.
    // The default (TUI-only) build never touches the webview toolchain.
    #[cfg(feature = "desktop")]
    tauri_build::build();
}
