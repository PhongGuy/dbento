//! Embeds the application icon into Windows builds; a no-op for every other target.

const ICON: &str = "../../assets/icon/generated/dbento.ico";

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    // The build script runs on the host, so the target comes from cargo's cfg variable.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon(ICON)
            .compile()
            .expect("embed the Windows icon resource");
    }
}
