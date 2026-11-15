//! Pile du thread principal agrandie sous Windows (64 Mo réservés, pas alloués) : la
//! compilation des shaders par DirectX (compilateur FXC) se fait sur ce thread et peut
//! dépasser la pile par défaut de 1 Mo avec les gros shaders (reflets).
//!
//! Exporte aussi `NvOptimusEnablement` et `AmdPowerXpressRequestHighPerformance` (définis
//! dans `main.rs`) : sur un portable à deux cartes, les pilotes NVIDIA et AMD lancent alors
//! le jeu sur la carte dédiée avec leur profil « hautes performances ».

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") {
        // Nom, éditeur et version dans l'exécutable (même numéro que `update::VERSION`).
        let update = std::fs::read_to_string("src/update.rs").unwrap_or_default();
        let version = update
            .lines()
            .find_map(|l| l.strip_prefix("pub const VERSION: u32 = "))
            .map(|v| v.trim_end_matches(';').trim().to_string())
            .unwrap_or_else(|| "0".into());
        println!("cargo:rerun-if-changed=src/update.rs");
        println!("cargo:rerun-if-changed=windows/minelib.rc");
        let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("minelib.rc");
        let rc = std::fs::read_to_string("windows/minelib.rc").unwrap_or_default();
        std::fs::write(&out, rc.replace("@V@", &version)).unwrap();
        if let Err(e) = embed_resource::compile(&out, embed_resource::NONE).manifest_optional() {
            println!("cargo:warning=informations de l'exécutable absentes : {e}");
        }
        if target.contains("msvc") {
            println!("cargo:rustc-link-arg-bins=/STACK:67108864");
            println!("cargo:rustc-link-arg-bins=/EXPORT:NvOptimusEnablement,DATA");
            println!("cargo:rustc-link-arg-bins=/EXPORT:AmdPowerXpressRequestHighPerformance,DATA");
        } else {
            println!("cargo:rustc-link-arg-bins=-Wl,--stack,67108864");
            let def =
                std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("gpu_exports.def");
            std::fs::write(&def, "EXPORTS\n    NvOptimusEnablement DATA\n    AmdPowerXpressRequestHighPerformance DATA\n").unwrap();
            println!("cargo:rustc-link-arg-bins={}", def.display());
        }
    }
}
