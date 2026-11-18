// Minelib — (v194) sous l'eau. Ce que la caméra voit sous la surface de l'eau est bleuté :
// le rouge et le vert s'éteignent avec la distance parcourue sous l'eau (absorption, filtre
// multiplié), et une brume bleue s'ajoute (diffusion). Seulement sous la ligne de l'eau : un
// pixel est sous l'eau quand son point sur le plan proche de la caméra est sous la surface.
// La caméra à moitié dans l'eau voit donc la ligne traverser l'image. Dessiné après l'image
// anticrénelée, la main et le bras, avant l'interface, en passes triées par la profondeur de
// l'interface : 0 (rien devant) : le monde, avec la distance lue dans sa profondeur ; sinon
// la main et le bras, tout près de l'œil. Pour chacun, d'abord le filtre (mélange
// multiplicatif), puis la brume (mélange additif).

struct Water {
    // Vue-projection sans le décalage du TAA (l'image anticrénelée n'en a pas).
    view_proj: mat4x4<f32>,
    // Œil (xyz), hauteur de la surface de l'eau au-dessus de lui (w).
    cam: vec4<f32>,
    // Couleur de la brume (linéaire, déjà assombrie la nuit), sa densité par bloc.
    color: vec4<f32>,
    // Absorption par bloc (rouge, vert, bleu), distance de la main et du bras (blocs).
    absorb: vec4<f32>,
    // Filtre au plus près (rgb : bleuté même tout contre l'œil).
    near: vec4<f32>,
    // zw : taille de l'image à l'écran (pixels).
    misc: vec4<f32>,
};

@group(0) @binding(0) var<uniform> w: Water;
@group(0) @binding(1) var scene_depth: texture_depth_2d;

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

/// Rayon de la caméra par le point `ndc`, à l'échelle d'une unité de profondeur de vue. Tiré
/// des lignes de la matrice, comme `dl_view_ray` (dynlights.wgsl) : jamais d'inverse de la
/// vue-projection (voir les « Pièges connus » du README).
fn view_ray(m: mat4x4<f32>, ndc: vec2<f32>) -> vec3<f32> {
    let rx = vec3<f32>(m[0][0], m[1][0], m[2][0]);
    let ry = vec3<f32>(m[0][1], m[1][1], m[2][1]);
    let rw = vec3<f32>(m[0][3], m[1][3], m[2][3]);
    let d = cross(rx - ndc.x * rw, ry - ndc.y * rw);
    return d / dot(rw, d);
}

/// Rayon du pixel `frag` (écran), et s'il part sous l'eau (son point du plan proche est
/// sous la surface).
struct Pixel {
    ray: vec3<f32>,
    under: bool,
};

fn pixel(frag: vec2<f32>) -> Pixel {
    let uv = frag / max(w.misc.zw, vec2<f32>(1.0));
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let ray = view_ray(w.view_proj, ndc);
    let near_y = w.cam.y + ray.y * w.view_proj[3][2];
    return Pixel(ray, near_y < w.cam.w);
}

/// Filtre de l'eau après `d` blocs : ce qui passe, par couleur.
fn water_filter(d: f32) -> vec3<f32> {
    return w.near.rgb * exp(-w.absorb.rgb * max(d, 0.0));
}

/// Brume ajoutée après `d` blocs.
fn water_haze(d: f32) -> vec3<f32> {
    return w.color.rgb * (1.0 - exp(-w.color.w * max(d, 0.0)));
}

/// Distance parcourue sous l'eau par le rayon du pixel `frag` jusqu'au monde : jusqu'à sa
/// surface (profondeur), ou jusqu'à la surface de l'eau si le rayon remonte et en sort avant.
fn world_distance(frag: vec2<f32>, ray: vec3<f32>) -> f32 {
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let q = vec2<i32>(clamp(frag * dims / max(w.misc.zw, vec2<f32>(1.0)), vec2<f32>(0.0), dims - 1.0));
    let depth = textureLoad(scene_depth, q, 0);
    let len = max(length(ray), 1e-6);
    var d = select(1e4, len * (w.view_proj[3][2] / depth), depth > 0.0);
    let dir = ray / len;
    if dir.y > 1e-4 && w.cam.y < w.cam.w {
        d = min(d, (w.cam.w - w.cam.y) / dir.y);
    }
    return d;
}

@fragment
fn fs_world_filter(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = pixel(frag.xy);
    if !px.under {
        discard;
    }
    return vec4<f32>(water_filter(world_distance(frag.xy, px.ray)), 1.0);
}

@fragment
fn fs_world_haze(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = pixel(frag.xy);
    if !px.under {
        discard;
    }
    return vec4<f32>(water_haze(world_distance(frag.xy, px.ray)), 1.0);
}

/// Main et bras (sous la ligne de l'eau) : à leur distance de l'œil.
@fragment
fn fs_hand_filter(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    if !pixel(frag.xy).under {
        discard;
    }
    return vec4<f32>(water_filter(w.absorb.w), 1.0);
}

@fragment
fn fs_hand_haze(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    if !pixel(frag.xy).under {
        discard;
    }
    return vec4<f32>(water_haze(w.absorb.w), 1.0);
}
