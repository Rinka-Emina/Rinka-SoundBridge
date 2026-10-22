fn main() {
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg=/IGNORE:4104");
        }
        if std::path::Path::new("icon.ico").exists() {
            let mut res = winres::WindowsResource::new();
            res.set_icon("icon.ico");
            res.set("ProductName", "Rinka SoundBridge v2.0");
            res.set(
                "FileDescription",
                "Rinka SoundBridge v2.0 (Rust Native) - WASAPI Shared/Exclusive, ASIO & VST3 Host",
            );
            let _ = res.compile();
        }
    }
}
