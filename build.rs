fn main() {
    // 只有启用 `desktop` feature 时才让 Tauri 生成上下文 / 资源绑定。
    // 默认（TUI-only）构建完全不触碰 webview 工具链。
    #[cfg(feature = "desktop")]
    tauri_build::build();
}
