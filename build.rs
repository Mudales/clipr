// Windows: embed the app icon in clipr.exe (Explorer, taskbar, Start menu).
fn main() {
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/clipr.ico");
        res.compile().expect("embedding the Windows icon");
    }
    println!("cargo:rerun-if-changed=assets/clipr.ico");
}
