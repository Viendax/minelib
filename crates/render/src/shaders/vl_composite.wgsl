// Minelib — lumière volumétrique (demi-résolution) ajoutée à l'image du monde en pleine
// résolution. Chaque pixel mélange les quatre échantillons voisins, pondérés par leur
// position et par la ressemblance de leur profondeur avec la sienne : les rayons ne bavent
// pas d'un objet proche sur le fond (ni l'inverse).

@group(0) @binding(0) var vl_tex: texture_2d<f32>;
@group(0) @binding(1) var full_depth: texture_depth_2d;

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_composite(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let fd = vec2<i32>(textureDimensions(full_depth));
    let d = textureLoad(full_depth, clamp(vec2<i32>(floor(frag.xy)), vec2<i32>(0), fd - 1), 0);
    let hd = vec2<i32>(textureDimensions(vl_tex));
    let f = frag.xy * 0.5 - 0.5;
    let i0 = vec2<i32>(floor(f));
    let fw = f - floor(f);
    var acc = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var k = 0; k < 4; k++) {
        let o = vec2<i32>(k & 1, k >> 1);
        let q = clamp(i0 + o, vec2<i32>(0), hd - 1);
        let s = textureLoad(vl_tex, q, 0);
        let bx = select(1.0 - fw.x, fw.x, o.x == 1);
        let by = select(1.0 - fw.y, fw.y, o.y == 1);
        // Profondeurs en Z inversé (∝ 1 / distance) : écart relatif ≈ écart relatif des
        // distances.
        let dz = abs(s.a - d) / max(max(s.a, d), 1e-6);
        let w = bx * by * exp(-dz * 24.0) + 1e-5 * bx * by;
        acc += s.rgb * w;
        wsum += w;
    }
    return vec4<f32>(acc / max(wsum, 1e-6), 0.0);
}
