//! Embeds the application icon and version information in the Windows
//! executable, so Explorer, the taskbar and shortcuts show the postmortem
//! icon instead of the default one.

fn main() {
    println!("cargo::rerun-if-changed=../../assets/brand/postmortem.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/brand/postmortem.ico");
        if let Err(e) = res.compile() {
            println!("cargo::warning=could not embed the Windows icon: {e}");
        }
    }
}
