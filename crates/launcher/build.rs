//! Nom, éditeur et description dans l'exécutable du lanceur (Propriétés → Détails sous
//! Windows) : un exécutable qui dit ce qu'il est est moins souvent pris pour un virus.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rerun-if-changed=windows/launcher.rc");
    if target.contains("windows") {
        if let Err(e) =
            embed_resource::compile("windows/launcher.rc", embed_resource::NONE).manifest_optional()
        {
            println!("cargo:warning=informations de l'exécutable absentes : {e}");
        }
    }
}
