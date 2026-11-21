// Minelib — ce que le monde opaque et les entités laissent pour l'éclairage différé des
// lumières dynamiques (dyn_deferred.wgsl), en deux images RGBA8 :
//   albédo : rgb = albédo (sRGB à l'écriture), a = brillance × 0,99 (1 : entité) ;
//   surface : rg = normale (octaèdre), b = poids de la lumière diffuse (brouillard,
//             reflets, occlusion, bloc visé), a = poids des reflets (6 bits) + métal (bit 2).
// Un pixel qui ne prend pas les lumières (ciel, flamme) garde 0 partout.

/// Normale → deux valeurs 0..1 (projection sur l'octaèdre).
fn gb_oct_encode(n: vec3<f32>) -> vec2<f32> {
    let m = n / max(abs(n.x) + abs(n.y) + abs(n.z), 1e-6);
    var e = m.xy;
    if m.z < 0.0 {
        e = (1.0 - abs(m.yx)) * select(vec2<f32>(-1.0), vec2<f32>(1.0), m.xy >= vec2<f32>(0.0));
    }
    return e * 0.5 + 0.5;
}

/// Inverse de `gb_oct_encode`.
fn gb_oct_decode(v: vec2<f32>) -> vec3<f32> {
    let e = v * 2.0 - 1.0;
    var n = vec3<f32>(e, 1.0 - abs(e.x) - abs(e.y));
    let t = max(-n.z, 0.0);
    n.x += select(t, -t, n.x >= 0.0);
    n.y += select(t, -t, n.y >= 0.0);
    return normalize(n);
}

/// Poids des reflets (0..1) et métal (reflet teinté par l'albédo) dans un octet.
fn gb_pack_spec(w: f32, metal: bool) -> f32 {
    let q = u32(round(clamp(w, 0.0, 1.0) * 63.0));
    return f32(q * 4u + select(0u, 2u, metal)) / 255.0;
}

/// Inverse de `gb_pack_spec` : (poids, métal 0 ou 1).
fn gb_unpack_spec(a: f32) -> vec2<f32> {
    let u = u32(round(a * 255.0));
    return vec2<f32>(f32(u >> 2u) / 63.0, select(0.0, 1.0, (u & 2u) != 0u));
}

/// (v215) Voile noir sur la lumière des lumières dynamiques qui tombe sur une entité (un
/// joueur, son skin) d'albédo `albedo` : plus le pixel est sombre, moins elle s'y voit. Un
/// pixel blanc la prend toute, comme avant ; un pixel noir, la moitié (la lumière d'un
/// projecteur faisait briller en rose un skin sombre). Clarté perçue : la luminance remise
/// en sRGB.
fn gb_dark_veil(albedo: vec3<f32>) -> f32 {
    let lum = clamp(dot(albedo, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
    return 1.0 - 0.5 * (1.0 - pow(lum, 1.0 / 2.2));
}
