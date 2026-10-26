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

    // Dictation needs the microphone, and macOS gates that on a usage description (TCC). A
    // bundled `.app` reads it from its Info.plist, but `cargo desktop` runs the bare binary --
    // for those, macOS reads the `__TEXT,__info_plist` Mach-O section instead. Without either,
    // `getUserMedia` is rejected with `NotAllowedError` and the composer just says
    // "Microphone permission denied." (see `docs/voice-input.md`).
    #[cfg(all(target_os = "macos", feature = "desktop"))]
    {
        let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Info.plist");
        println!("cargo:rerun-if-changed=Info.plist");
        println!("cargo:rerun-if-changed=Entitlements.plist");
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
