//! Embeds the app icon (the C# app's icon, `assets/app.ico`) in the Windows executable and plugin.

fn main() {
    println!("cargo:rerun-if-changed=../assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // resource id 1: also loaded at runtime for the standalone window's title bar and taskbar
        res.set_icon_with_id("../assets/app.ico", "1");
        res.set("ProductName", "Jwala");
        res.set("FileDescription", "Jwala amp & effects rig");
        if let Err(e) = res.compile() {
            println!("cargo:warning=couldn't embed the app icon: {e}");
        }
    }
}
