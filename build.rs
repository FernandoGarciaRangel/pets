fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Dependência dos Common Controls v6: sem ela o slider e os botões
        // aparecem com o visual do Windows 95.
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
    // Ícone do app (a logo da raposa).
    println!("cargo:rerun-if-changed=pets.rc");
    println!("cargo:rerun-if-changed=assets/pets.ico");
    embed_resource::compile("pets.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
