fn main() {
    // Embeds the app icon (resource ID 1) into the executable; the tray icon is
    // loaded from the same resource at runtime.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .set("FileDescription", "Big Picture Audio")
            .set("ProductName", "Big Picture Audio")
            .compile()
            .expect("failed to embed Windows resources");
    }
}
