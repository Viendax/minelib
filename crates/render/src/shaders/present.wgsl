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
// (v295) Profondeur de la scène de cette image (0 : le ciel), à la résolution interne.
@group(0) @binding(1) var scene_depth: texture_depth_2d;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

/// (v295) « Adapte ce qui ne va pas réalistement, de manière optimisée pour nos FPS » : une
/// lumière forte et colorée (passée par du verre coloré, ou colorée elle-même) blanchissait
/// ce qu'elle éclairait. Chaque canal était coupé à 1 : dès que le plus faible dépassait 1,
/// tout devenait blanc, quelle que soit la couleur. Au-delà de 1, la couleur garde maintenant
/// sa teinte (le canal le plus fort à 1, les autres dans les mêmes proportions), et ne
/// blanchit que peu à peu quand la lumière devient très forte, comme pour l'œil ou une
/// pellicule. Rien ne change jusqu'à 1 ; le passage se fait en douceur entre 1 et 3 (un
/// blanc à peine trop fort reste comme avant). Quelques opérations par pixel.
fn keep_hue(c: vec3<f32>) -> vec3<f32> {
    let m = max(c.r, max(c.g, c.b));
    if m <= 1.0 {
        return c;
    }
    let white = 1.0 - 1.0 / (1.0 + (m - 1.0) * 0.02);
    let kept = mix(c / m, vec3<f32>(1.0), white);
    return mix(min(c, vec3<f32>(1.0)), kept, smoothstep(1.0, 3.0, m));
}

/// Copie + léger renforcement de la netteté (compense la douceur du TAA). On ne touche
/// qu'à la luminosité, sans dépasser les voisins : pas de franges colorées. (v295) Puis les
/// couleurs trop fortes gardent leur teinte (`keep_hue`), sauf le ciel (le soleil n'est pas
/// touché).
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
    var o = c * k;
    // Le pixel de la scène (la profondeur peut être à une échelle de rendu réduite).
    let dsize = vec2<i32>(textureDimensions(scene_depth));
    let q = clamp(
        vec2<i32>((vec2<f32>(p) + 0.5) * vec2<f32>(dsize) / vec2<f32>(size)),
        vec2<i32>(0),
        dsize - 1,
    );
    if textureLoad(scene_depth, q, 0) > 0.0 {
        o = keep_hue(o);
    }
    return vec4<f32>(clamp(o, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
