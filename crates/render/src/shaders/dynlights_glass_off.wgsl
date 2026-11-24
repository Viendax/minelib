// Minelib — (v268) shaders des lumières sans la grille de blocs (monde, entités, rayons,
// pluie…) : la teinte du verre sur une surface vient de la carte de teinte seule (voir
// `dl_surface_tint` de dynlights.wgsl ; le suivi exact est dans dynlights_glass.wgsl, pour
// l'éclairage différé).
fn dl_glass_exact(o: vec3<f32>, q: vec3<f32>, t_start: f32, t_stop: f32, own: bool) -> vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 0.0);
}

/// (v278) Sans la grille de blocs : la carte d'ombre seule (voir `dl_block_edge` de
/// dynlights_glass.wgsl).
fn dl_block_edge(L: DynLight, fr: DlFrame, p: vec3<f32>, n: vec3<f32>, dir: vec3<f32>, range: f32) -> f32 {
    return -1.0;
}
