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
