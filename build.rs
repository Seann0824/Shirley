fn main() {
    // Tauri 2.7 只为 `tauri.conf.json` / `capabilities` 发 `rerun-if-changed`，
    // **不管 `bundle.icon`**。而 macOS 的 dev 构建会把 `icons/icon.icns` 的字节
    // 直接嵌进二进制、启动时再设成 Dock 图标——所以少了这一行，重新导出图标后
    // `cargo build` 是空转，Dock 永远停在旧图标上。
    println!("cargo:rerun-if-changed=icons");

    // 只有启用 `desktop` feature 时才让 Tauri 生成上下文 / 资源绑定。
    // 默认（TUI-only）构建完全不触碰 webview 工具链。
    #[cfg(feature = "desktop")]
    tauri_build::build();
}
