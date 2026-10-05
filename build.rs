//! Embeds the app icon and version info into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/claudiu.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/claudiu.ico");
        res.set("ProductName", "Claudiu");
        res.set("FileDescription", "Claudiu - Claude Code workspace");
        if let Err(e) = res.compile() {
            // Never fail the build over cosmetics (e.g. no resource compiler on this machine).
            println!("cargo:warning=could not embed icon: {e}");
        }
    }
}
