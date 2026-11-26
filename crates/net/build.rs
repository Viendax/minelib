//! (v347) Noms des genres de messages (`Request`, `Response`), dans l'ordre de `proto.rs` :
//! le numéro d'une variante dans un message (postcard) donne son nom au journal
//! (`traffic`). Rien à tenir à jour à la main.

use std::io::Write;

fn variants(src: &str, name: &str) -> Vec<String> {
    let head = format!("pub enum {name} {{");
    let mut out = Vec::new();
    let mut inside = false;
    for line in src.lines() {
        if !inside {
            inside = line.trim_end() == head;
            continue;
        }
        if line.starts_with('}') {
            break;
        }
        // Une variante : quatre espaces puis une majuscule (les champs sont plus loin).
        if let Some(rest) = line.strip_prefix("    ") {
            if rest.starts_with(|c: char| c.is_ascii_uppercase()) {
                out.push(
                    rest.chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect(),
                );
            }
        }
    }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=src/proto.rs");
    let src = std::fs::read_to_string("src/proto.rs").expect("src/proto.rs");
    let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("genres.rs");
    let mut f = std::fs::File::create(out).expect("genres.rs");
    for (konst, e) in [("REQUEST_KINDS", "Request"), ("RESPONSE_KINDS", "Response")] {
        let v = variants(&src, e);
        assert!(!v.is_empty(), "pub enum {e} introuvable dans proto.rs");
        writeln!(f, "pub const {konst}: &[&str] = &{v:?};").expect("genres.rs");
    }
}
