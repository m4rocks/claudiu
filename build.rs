//! Embeds the app icon and version info into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/m4code.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/m4code.ico");
        res.set("ProductName", "m4code");
        res.set("FileDescription", "m4code - Claude Code workspace");
        if let Err(e) = res.compile() {
            // Never fail the build over cosmetics (e.g. no resource compiler on this machine).
            println!("cargo:warning=could not embed icon: {e}");
        }
    }
}
