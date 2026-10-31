use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("icon.o");
    let status = Command::new("windres")
        .current_dir(manifest)
        .arg("assets/icon.rc")
        .arg("-O")
        .arg("coff")
        .arg("-o")
        .arg(&out)
        .status();
    match status {
        Ok(status) if status.success() => println!("cargo:rustc-link-arg={}", out.display()),
        _ => println!("cargo:warning=windres unavailable; exe keeps the default icon"),
    }
}
