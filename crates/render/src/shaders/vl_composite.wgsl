// Minelib — lumière volumétrique (demi- ou quart de résolution) ajoutée à l'image du monde
// en pleine résolution. Chaque pixel mélange les quatre échantillons voisins, pondérés par
// leur position et par la ressemblance de leur profondeur avec la sienne : les rayons ne
// bavent pas d'un objet proche sur le fond (ni l'inverse).

@group(0) @binding(0) var vl_tex: texture_2d<f32>;
@group(0) @binding(1) var full_depth: texture_depth_2d;
// Cases d'écran éclairées (rectangles en NDC) : seulement là qu'il peut y avoir des rayons
// (ailleurs, la recomposition n'ajoutait que des zéros). La marche a rempli ces cases,
// élargies de quelques pixels : les voisins lus ici en font partie.
@group(0) @binding(2) var<storage, read> lit_rects: array<vec4<f32>>;

@vertex
fn vs_tiles(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> @builtin(position) vec4<f32> {
    // Coins (0,0) (1,0) (0,1) (0,1) (1,0) (1,1) : bits de x = 110010, de y = 101100.
    let r = lit_rects[ii];
    let cx = f32((50u >> vi) & 1u);
    let cy = f32((44u >> vi) & 1u);
    return vec4<f32>(mix(r.x, r.z, cx), mix(r.y, r.w, cy), 0.0, 1.0);
}

/// Poids d'un des quatre voisins : sa place (bilinéaire) et la ressemblance de sa profondeur
/// avec celle du pixel. Profondeurs en Z inversé (∝ 1 / distance) : écart relatif ≈ écart
/// relatif des distances.
fn vl_weight(s: vec4<f32>, d: f32, b: f32) -> f32 {
    let dz = abs(s.a - d) / max(max(s.a, d), 1e-6);
    return b * exp(-dz * 24.0) + 1e-5 * b;
}

@fragment
fn fs_composite(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let fd = vec2<i32>(textureDimensions(full_depth));
    let hd = vec2<i32>(textureDimensions(vl_tex));
    // Demi- ou quart de résolution : rapport des tailles des deux images.
    let ratio = vec2<f32>(fd) / vec2<f32>(hd);
    let f = frag.xy / ratio - 0.5;
    let i0 = vec2<i32>(floor(f));
    let fw = f - floor(f);
    let hi = hd - 1;
    let s00 = textureLoad(vl_tex, clamp(i0, vec2<i32>(0), hi), 0);
    let s10 = textureLoad(vl_tex, clamp(i0 + vec2<i32>(1, 0), vec2<i32>(0), hi), 0);
    let s01 = textureLoad(vl_tex, clamp(i0 + vec2<i32>(0, 1), vec2<i32>(0), hi), 0);
    let s11 = textureLoad(vl_tex, clamp(i0 + vec2<i32>(1, 1), vec2<i32>(0), hi), 0);
    // Aucun rayon chez les quatre voisins (hors des faisceaux) : rien à ajouter, quels que
    // soient les poids. Ni profondeur à lire, ni exponentielles.
    let m = max(max(s00.rgb, s10.rgb), max(s01.rgb, s11.rgb));
    if max(m.x, max(m.y, m.z)) <= 0.0 {
        return vec4<f32>(0.0);
    }
    let d = textureLoad(full_depth, clamp(vec2<i32>(floor(frag.xy)), vec2<i32>(0), fd - 1), 0);
    let w00 = vl_weight(s00, d, (1.0 - fw.x) * (1.0 - fw.y));
    let w10 = vl_weight(s10, d, fw.x * (1.0 - fw.y));
    let w01 = vl_weight(s01, d, (1.0 - fw.x) * fw.y);
    let w11 = vl_weight(s11, d, fw.x * fw.y);
    let acc = s00.rgb * w00 + s10.rgb * w10 + s01.rgb * w01 + s11.rgb * w11;
    return vec4<f32>(acc / max(w00 + w10 + w01 + w11, 1e-6), 0.0);
}
