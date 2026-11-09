// Texte et panneaux 2D de l'écran de debug.
// Les sommets arrivent déjà en coordonnées écran normalisées (NDC).

@group(0) @binding(0) var font: texture_2d<f32>;
@group(0) @binding(1) var font_sampler: sampler;

struct VIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_text(v: VIn) -> VOut {
    var o: VOut;
    o.clip = vec4<f32>(v.pos, 0.0, 1.0);
    o.uv = v.uv;
    o.color = v.color;
    return o;
}

@fragment
fn fs_text(i: VOut) -> @location(0) vec4<f32> {
    // Atlas RGBA : police blanche (alpha = couverture), images du pack.
    return textureSample(font, font_sampler, i.uv) * i.color;
}
