// Minelib — fumigènes en volume (voir smoke.rs).

// Début des uniformes du monde.
struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    fog: vec4<f32>,
    sun: vec4<f32>,
    sel_min: vec4<f32>,
    sel_max: vec4<f32>,
    light: vec4<f32>,
};

struct Smoke {
    origin: vec4<f32>,   // xyz = coin bas, w = taille d'une case
    size: vec4<f32>,     // xyz = taille de la boîte, w = front de la fumée
    params: vec4<f32>,   // x = tunnels, y = âge, z = pas max, w = longueur d'un pas
    tunnel_a: array<vec4<f32>, 16>, // w = rayon
    tunnel_b: array<vec4<f32>, 16>,
    // Boîte serrée autour des cases que la fumée peut atteindre (monde).
    box_min: vec4<f32>,
    box_max: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var<uniform> s: Smoke;
@group(1) @binding(1) var fill: texture_3d<f32>;
@group(1) @binding(2) var noise: texture_3d<f32>;
@group(1) @binding(3) var fill_sampler: sampler;
@group(1) @binding(4) var noise_sampler: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
};

/// Boîte du nuage : 6 faces de 2 triangles, tournées vers l'extérieur.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
    let face = vi / 6u;
    let k = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u)[vi % 6u];
    let sg = select(-1.0, 1.0, face % 2u == 0u);
    var n = vec3<f32>(0.0);
    var u = vec3<f32>(0.0);
    var v = vec3<f32>(0.0);
    if face < 2u {
        n = vec3<f32>(sg, 0.0, 0.0);
        u = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), sg > 0.0);
        v = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), sg > 0.0);
    } else if face < 4u {
        n = vec3<f32>(0.0, sg, 0.0);
        u = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), sg > 0.0);
        v = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0), sg > 0.0);
    } else {
        n = vec3<f32>(0.0, 0.0, sg);
        u = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), sg > 0.0);
        v = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), sg > 0.0);
    }
    let c = array<vec2<f32>, 4>(vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0))[k];
    let unit = 0.5 + 0.5 * (n + c.x * u + c.y * v);
    let p = s.box_min.xyz + unit * (s.box_max.xyz - s.box_min.xyz);
    var o: VOut;
    o.clip = g.view_proj * vec4<f32>(p, 1.0);
    o.world = p;
    return o;
}

fn hash12(p: vec2<f32>) -> f32 {
    var q = fract(vec3<f32>(p.xyx) * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return fract((q.x + q.y) * q.z);
}

/// Distance la plus courte entre le rayon o + d·t (t dans [t0, t1]) et le segment [a, b].
fn ray_segment(o: vec3<f32>, d: vec3<f32>, t0: f32, t1: f32, a: vec3<f32>, b: vec3<f32>) -> f32 {
    let e = b - a;
    let w = o - a;
    let ee = max(dot(e, e), 1e-6);
    let de = dot(d, e);
    let denom = ee - de * de;
    var t = t0;
    if denom > 1e-5 {
        t = (de * dot(w, e) - ee * dot(w, d)) / denom;
    }
    t = clamp(t, t0, t1);
    let sp = clamp(dot(o + d * t - a, e) / ee, 0.0, 1.0);
    let q = a + e * sp;
    // Puis le point du rayon le plus proche de ce point du segment.
    let t2 = clamp(dot(q - o, d), t0, t1);
    return length(o + d * t2 - q);
}

fn seg_dist(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> f32 {
    let e = b - a;
    let t = clamp(dot(p - a, e) / max(dot(e, e), 1e-6), 0.0, 1.0);
    return length(a + e * t - p);
}

/// Fumée au point `p` : x = densité 0..1 (front, volutes), y = 1 dans un mur, z = moment
/// où la fumée arrive là (0 centre … 1 bord, plus : jamais).
fn field(p: vec3<f32>, front: f32, age: f32) -> vec3<f32> {
    let raw = textureSampleLevel(fill, fill_sampler, (p - s.origin.xyz) / s.size.xyz, 0.0).rg;
    // Moment d'arrivée rangé ×0,8 (voir equip::SMOKE_REACH_SCALE) : 1,25 = jamais.
    let f = vec2<f32>(raw.r * 1.25, raw.g);
    if f.g > 0.5 || f.r > front - 0.04 {
        return vec3<f32>(0.0, step(0.5, f.g), f.r);
    }
    // Le cœur reste plein : les volutes ne creusent que la moitié extérieure du nuage.
    let edge = smoothstep(0.25, 0.85, f.r);
    if edge <= 0.0 || front - f.r > 0.6 {
        return vec3<f32>(1.0, 0.0, f.r);
    }
    // Bouffées : bruit « en boules » (creux entre les volutes) sur deux tailles qui
    // dérivent lentement (la fumée monte et tourne).
    let drift = vec3<f32>(age * 0.03, -age * 0.05, age * 0.02);
    let n1 = textureSampleLevel(noise, noise_sampler, p * 0.17 + drift, 0.0).r;
    let n2 = textureSampleLevel(noise, noise_sampler, p * 0.5 - drift * 1.6, 0.0).r;
    let b = abs(n1 * 2.0 - 1.0) * 0.62 + abs(n2 * 2.0 - 1.0) * 0.38;
    let erode = (0.05 + b * 0.42) * edge;
    return vec3<f32>(clamp((front - f.r - erode) * 8.0, 0.0, 1.0), 0.0, f.r);
}

fn march(frag: vec4<f32>, world: vec3<f32>) -> vec4<f32> {
    let o = g.cam_pos.xyz;
    let d = normalize(world - o);
    // Entrée et sortie de la boîte.
    let inv = 1.0 / select(d, vec3<f32>(1e-6), abs(d) < vec3<f32>(1e-6));
    let ta = (s.box_min.xyz - o) * inv;
    let tb = (s.box_max.xyz - o) * inv;
    let t_in = max(max(max(min(ta.x, tb.x), min(ta.y, tb.y)), min(ta.z, tb.z)), 0.0);
    let t_out = min(min(max(ta.x, tb.x), max(ta.y, tb.y)), max(ta.z, tb.z));
    if t_out <= t_in {
        discard;
    }
    // Tunnels proches du rayon seulement.
    var mask = 0u;
    let nt = u32(s.params.x);
    for (var k = 0u; k < 16u; k++) {
        if k >= nt {
            break;
        }
        let a = s.tunnel_a[k];
        if a.w > 0.0 && ray_segment(o, d, t_in, t_out, a.xyz, s.tunnel_b[k].xyz) < a.w + 0.1 {
            mask |= 1u << k;
        }
    }
    let front = s.size.w;
    let age = s.params.y;
    let dt = s.params.w;
    let steps = i32(s.params.z);
    let sun = normalize(g.sun.xyz);
    let day = g.light.x;
    let amb = mix(0.05, 1.0, day);
    var t = t_in + dt * hash12(frag.xy + vec2<f32>(age * 37.0, age * 11.0));
    var trans = 1.0;
    var col = vec3<f32>(0.0);
    // Éclairage de la surface du nuage, calculé au premier contact (normale des volutes).
    var light = -1.0;
    var t_hit = 0.0;
    for (var i = 0; i < 160; i++) {
        if i >= steps || t > t_out || trans < 0.015 {
            break;
        }
        let p = o + d * t;
        let fd = field(p, front, age);
        // Mur (bloc plein) : la fumée derrière est cachée.
        if fd.y > 0.5 {
            break;
        }
        var dens = fd.x;
        if dens > 0.001 && mask != 0u {
            for (var k = 0u; k < 16u; k++) {
                if (mask & (1u << k)) != 0u {
                    let a = s.tunnel_a[k];
                    dens *= clamp((seg_dist(p, a.xyz, s.tunnel_b[k].xyz) - a.w) / 0.06, 0.0, 1.0);
                }
            }
        }
        if dens > 0.001 {
            if light < 0.0 && t < 0.3 {
                // Œil dans la fumée : brouillard uniforme, pas besoin du relief.
                light = amb * 0.62;
                t_hit = t;
            }
            if light < 0.0 {
                // Bord exact de la fumée entre le pas précédent et celui-ci (sinon les
                // pas dessinent des bandes), puis la normale : la fumée s'épaissit vers
                // l'intérieur.
                var lo = t - dt;
                var hi = t;
                for (var b = 0; b < 2; b++) {
                    let mid = 0.5 * (lo + hi);
                    if field(o + d * mid, front, age).x > 0.05 {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                let q = o + d * hi;
                let e = 0.2;
                let d0 = field(q, front, age).x;
                let gx = field(q + vec3<f32>(e, 0.0, 0.0), front, age).x - d0;
                let gy = field(q + vec3<f32>(0.0, e, 0.0), front, age).x - d0;
                let gz = field(q + vec3<f32>(0.0, 0.0, e), front, age).x - d0;
                var nrm = -vec3<f32>(gx, gy, gz);
                nrm = select(-d, normalize(nrm), dot(nrm, nrm) > 1e-6);
                // Ombre de la fumée elle-même vers le soleil.
                let sh = field(q + sun * 0.9, front, age).x;
                let sky = 0.5 + 0.5 * nrm.y;
                let direct = max(dot(nrm, sun), 0.0) * (1.0 - 0.6 * sh);
                light = amb * (0.52 + 0.26 * sky) + day * 0.32 * direct;
                t_hit = t;
            }
            // Cœur du nuage (plein, sans volutes) : pas plus longs, deux suffisent.
            let core = front - fd.z > 0.6 && mask == 0u;
            let step_len = select(dt, 0.24, core);
            let alpha = 1.0 - exp(-dens * 13.0 * step_len);
            // Plus profond dans le nuage : plus sombre (la lumière y arrive moins).
            let deep = exp(-(t - t_hit) * 1.2);
            let h = clamp((p.y - s.origin.y - 1.0) / 3.5, 0.0, 1.0);
            let c = vec3<f32>(0.40, 0.405, 0.415) * light * mix(0.75, 1.0, deep) * (0.85 + 0.15 * h);
            col += trans * alpha * c;
            trans *= 1.0 - alpha;
            t += step_len;
        } else {
            // Loin de la fumée : grands pas (le front est à au moins (reach − front) × ~4 m).
            t += clamp((fd.z - front) * 3.5, dt, 0.45);
        }
    }
    let a = 1.0 - trans;
    if a < 0.003 {
        discard;
    }
    return vec4<f32>(col, a);
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    return march(i.clip, i.world);
}

struct TaaOut {
    @location(0) color: vec4<f32>,
    @location(1) motion: vec2<f32>,
};

@fragment
fn fs_taa(i: VOut) -> TaaOut {
    var o: TaaOut;
    o.color = march(i.clip, i.world);
    o.motion = vec2<f32>(0.0);
    return o;
}
