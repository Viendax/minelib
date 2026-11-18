// Ombres par lancer de rayons absentes de ce shader (voir dl_rt.wgsl) : la carte d'ombre
// décide toujours.
fn dl_rt_shadow(L: DynLight, p: vec3<f32>, n: vec3<f32>, noise: f32, frag: vec2<f32>) -> f32 {
    return -1.0;
}
