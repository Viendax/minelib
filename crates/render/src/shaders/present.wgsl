// Minelib — copie de l'image anticrénelée à l'écran.

struct FsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> FsOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: FsOut;
    o.clip = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return o;
}

@group(0) @binding(0) var shown: texture_2d<f32>;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

/// Copie + léger renforcement de la netteté (compense la douceur du TAA). On ne touche
/// qu'à la luminosité, sans dépasser les voisins : pas de franges colorées.
@fragment
fn fs_present(i: FsOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(i.clip.xy);
    let size = vec2<i32>(textureDimensions(shown));
    let c = textureLoad(shown, p, 0).rgb;
    let a = textureLoad(shown, clamp(p + vec2<i32>(0, -1), vec2<i32>(0), size - 1), 0).rgb;
    let b = textureLoad(shown, clamp(p + vec2<i32>(0, 1), vec2<i32>(0), size - 1), 0).rgb;
    let l = textureLoad(shown, clamp(p + vec2<i32>(-1, 0), vec2<i32>(0), size - 1), 0).rgb;
    let r = textureLoad(shown, clamp(p + vec2<i32>(1, 0), vec2<i32>(0), size - 1), 0).rgb;
    let yc = luma(c);
    let ya = luma(a);
    let yb = luma(b);
    let yl = luma(l);
    let yr = luma(r);
    let lo = min(yc, min(min(ya, yb), min(yl, yr)));
    let hi = max(yc, max(max(ya, yb), max(yl, yr)));
    let target_y = clamp(yc + (yc * 4.0 - ya - yb - yl - yr) * 0.2, lo, hi);
    let k = select(1.0, target_y / yc, yc > 1e-4);
    return vec4<f32>(clamp(c * k, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
