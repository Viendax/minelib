//! Interface 2D : texte (police 8×8), écran de debug F3, menus et inventaire.
//!
//! Tout passe par un **atlas RGBA** unique : la police 8×8 (`font8x8`, domaine public)
//! dans le coin (0, 0), puis les éléments d'interface et les icônes du pack de ressources
//! (voir `textures::build_gui_atlas`). La case 127 de la police est pleine et sert aux
//! rectangles de couleur.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};

use crate::textures::{GuiAtlas, ATLAS_SIZE, FONT_CELL};
use crate::viewmodel::VmVertex;

const CELL: u32 = FONT_CELL;
const SOLID: u32 = 127;

/// Une ligne de texte avec sa couleur (RVBA, 0..1).
#[derive(Clone, Debug)]
pub struct Line {
    pub text: String,
    pub color: [f32; 4],
}

impl Line {
    pub const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    pub const TITLE: [f32; 4] = [1.0, 0.85, 0.35, 1.0];
    pub const DIM: [f32; 4] = [0.75, 0.80, 0.85, 1.0];
    pub const GOOD: [f32; 4] = [0.45, 0.95, 0.45, 1.0];
    pub const WARN: [f32; 4] = [1.0, 0.75, 0.3, 1.0];
    pub const BAD: [f32; 4] = [1.0, 0.4, 0.35, 1.0];

    pub fn new(text: impl Into<String>, color: [f32; 4]) -> Self {
        Self {
            text: text.into(),
            color,
        }
    }
    pub fn white(text: impl Into<String>) -> Self {
        Self::new(text, Self::WHITE)
    }
}

/// Contenu de l'écran de debug : une colonne alignée à gauche, une à droite.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    pub left: Vec<Line>,
    pub right: Vec<Line>,
}

/// Interface 2D décrite par le jeu : coordonnées en pixels, origine en haut à gauche.
/// Les images sont désignées par leur nom dans l'atlas (`gui/button`, `icon/block/3`…).
#[derive(Clone, Debug, Default)]
pub struct Ui {
    cmds: Vec<UiCmd>,
}

#[derive(Clone, Debug)]
enum UiCmd {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [f32; 4],
    },
    Text {
        x: f32,
        y: f32,
        scale: f32,
        color: [f32; 4],
        text: String,
        shadow: bool,
    },
    Image {
        name: String,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        tint: [f32; 4],
    },
    NineSlice {
        name: String,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        border: f32,
        scale: f32,
        tint: [f32; 4],
    },
    Tiled {
        name: String,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scale: f32,
        tint: [f32; 4],
    },
    Model(Vec<VmVertex>),
}

impl Ui {
    pub fn clear(&mut self) {
        self.cmds.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        self.cmds.push(UiCmd::Rect { x, y, w, h, color });
    }

    /// Texte en police 8×8 agrandie `scale` fois, avec ombre portée.
    pub fn text(&mut self, x: f32, y: f32, scale: f32, color: [f32; 4], text: impl Into<String>) {
        self.cmds.push(UiCmd::Text {
            x,
            y,
            scale,
            color,
            text: text.into(),
            shadow: true,
        });
    }

    /// Texte sans ombre (sur fond clair).
    pub fn text_flat(
        &mut self,
        x: f32,
        y: f32,
        scale: f32,
        color: [f32; 4],
        text: impl Into<String>,
    ) {
        self.cmds.push(UiCmd::Text {
            x,
            y,
            scale,
            color,
            text: text.into(),
            shadow: false,
        });
    }

    /// Image de l'atlas étirée sur le rectangle.
    pub fn image(&mut self, name: &str, x: f32, y: f32, w: f32, h: f32, tint: [f32; 4]) {
        self.cmds.push(UiCmd::Image {
            name: name.into(),
            x,
            y,
            w,
            h,
            tint,
        });
    }

    /// Image découpée en 9 (coins fixes de `border` pixels source, agrandis `scale` fois).
    #[allow(clippy::too_many_arguments)]
    pub fn nine_slice(
        &mut self,
        name: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        border: f32,
        scale: f32,
    ) {
        self.cmds.push(UiCmd::NineSlice {
            name: name.into(),
            x,
            y,
            w,
            h,
            border,
            scale,
            tint: [1.0; 4],
        });
    }

    /// Image répétée en mosaïque (fond de menu).
    #[allow(clippy::too_many_arguments)]
    pub fn tiled(
        &mut self,
        name: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        scale: f32,
        tint: [f32; 4],
    ) {
        self.cmds.push(UiCmd::Tiled {
            name: name.into(),
            x,
            y,
            w,
            h,
            scale,
            tint,
        });
    }

    /// Objet en 3D (torche animée…) : triangles dont `clip` = (x, y en pixels, profondeur
    /// 0..1 — plus grand = plus proche, 1). Dessiné à sa place dans l'ordre de l'interface.
    pub fn model(&mut self, vertices: Vec<VmVertex>) {
        self.cmds.push(UiCmd::Model(vertices));
    }

    /// Largeur en pixels d'un texte à l'échelle `scale`.
    pub fn text_width(text: &str, scale: f32) -> f32 {
        text.chars().count() as f32 * 8.0 * scale
    }
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct TextVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

pub(crate) struct OverlayRenderer {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    sprites: HashMap<String, [u32; 4]>,
    buffer: wgpu::Buffer,
    capacity: usize,
    count: u32,
    pub(crate) gpu_bytes: u64,
    /// Objets 3D de l'interface (convertis en coordonnées écran) et, pour chacun, le nombre
    /// de sommets 2D à dessiner avant lui et sa plage dans `models`.
    pub(crate) models: Vec<VmVertex>,
    /// Sommets de l'image précédente (mémoire réutilisée).
    scratch: Vec<TextVertex>,
    pub(crate) splits: Vec<(u32, std::ops::Range<u32>)>,
}

/// Police 8×8 en niveaux de gris, 128 × 128 (16 × 16 cases).
pub(crate) fn font_bitmap() -> Vec<u8> {
    use font8x8::legacy::{BASIC_LEGACY, LATIN_LEGACY};
    let mut px = vec![0u8; 128 * 128];
    for code in 0..256u32 {
        let glyph: [u8; 8] = match code {
            SOLID => [0xff; 8],
            0..=127 => BASIC_LEGACY[code as usize],
            160..=255 => LATIN_LEGACY[(code - 160) as usize],
            _ => [0; 8],
        };
        let (cx, cy) = ((code % 16) * CELL, (code / 16) * CELL);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) != 0 {
                    px[((cy + row as u32) * 128 + cx + col) as usize] = 255;
                }
            }
        }
    }
    px
}

/// Quads en coordonnées écran → sommets NDC.
struct Builder {
    verts: Vec<TextVertex>,
    w: f32,
    h: f32,
}

impl Builder {
    /// Quad (x0, y0)-(x1, y1) texturé par le rectangle atlas `uv` (pixels : x, y, l, h).
    fn quad(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, uv: [f32; 4], color: [f32; 4]) {
        let to_ndc = |x: f32, y: f32| [x / self.w * 2.0 - 1.0, 1.0 - y / self.h * 2.0];
        let a = ATLAS_SIZE as f32;
        let (u0, v0, u1, v1) = (
            uv[0] / a,
            uv[1] / a,
            (uv[0] + uv[2]) / a,
            (uv[1] + uv[3]) / a,
        );
        let v = [
            TextVertex {
                pos: to_ndc(x0, y0),
                uv: [u0, v0],
                color,
            },
            TextVertex {
                pos: to_ndc(x1, y0),
                uv: [u1, v0],
                color,
            },
            TextVertex {
                pos: to_ndc(x1, y1),
                uv: [u1, v1],
                color,
            },
            TextVertex {
                pos: to_ndc(x0, y1),
                uv: [u0, v1],
                color,
            },
        ];
        self.verts
            .extend_from_slice(&[v[0], v[1], v[2], v[2], v[3], v[0]]);
    }

    fn glyph(&mut self, x: f32, y: f32, size: f32, code: u32, color: [f32; 4]) {
        let uv = [
            (code % 16 * CELL) as f32,
            (code / 16 * CELL) as f32,
            CELL as f32,
            CELL as f32,
        ];
        self.quad(x, y, x + size, y + size, uv, color);
    }

    fn solid(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: [f32; 4]) {
        // Centre de la case pleine : évite tout bord transparent.
        let c = [
            (SOLID % 16 * CELL) as f32 + 2.0,
            (SOLID / 16 * CELL) as f32 + 2.0,
            4.0,
            4.0,
        ];
        self.quad(x0, y0, x1, y1, c, color);
    }

    fn text(&mut self, x: f32, y: f32, scale: f32, color: [f32; 4], text: &str, shadow: bool) {
        let g = 8.0 * scale;
        for (k, ch) in text.chars().enumerate() {
            let code = if (ch as u32) < 256 {
                ch as u32
            } else {
                '?' as u32
            };
            if code == ' ' as u32 {
                continue;
            }
            let gx = x + k as f32 * g;
            if shadow {
                self.glyph(
                    gx + scale,
                    y + scale,
                    g,
                    code,
                    [0.0, 0.0, 0.0, 0.85 * color[3]],
                );
            }
            self.glyph(gx, y, g, code, color);
        }
    }
}

impl OverlayRenderer {
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        depth: wgpu::TextureFormat,
        atlas: &GuiAtlas,
    ) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("interface"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("interface"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = Self::make_bind_group(device, queue, &bgl, &sampler, atlas);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("interface"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/text.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("interface"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_text"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TextVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
                }],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_text"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let capacity = 8192;
        let buffer = Self::create_buffer(device, capacity);
        Self {
            pipeline,
            bgl,
            sampler,
            bind_group,
            sprites: atlas.sprites.clone(),
            buffer,
            capacity,
            count: 0,
            gpu_bytes: Self::bytes(capacity),
            models: Vec::new(),
            splits: Vec::new(),
            scratch: Vec::with_capacity(8192),
        }
    }

    fn bytes(capacity: usize) -> u64 {
        (ATLAS_SIZE * ATLAS_SIZE * 4) as u64 + (capacity * std::mem::size_of::<TextVertex>()) as u64
    }

    fn make_bind_group(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bgl: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        atlas: &GuiAtlas,
    ) -> wgpu::BindGroup {
        use wgpu::util::DeviceExt;
        let texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("atlas interface"),
                size: wgpu::Extent3d {
                    width: ATLAS_SIZE,
                    height: ATLAS_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &atlas.image.data,
        );
        let view = texture.create_view(&Default::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("interface"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// Remplace l'atlas (changement de pack de ressources).
    pub(crate) fn set_atlas(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &GuiAtlas,
    ) {
        self.bind_group = Self::make_bind_group(device, queue, &self.bgl, &self.sampler, atlas);
        self.sprites = atlas.sprites.clone();
    }

    pub(crate) fn sprite_size(&self, name: &str) -> Option<(u32, u32)> {
        self.sprites.get(name).map(|r| (r[2], r[3]))
    }

    fn create_buffer(device: &wgpu::Device, vertices: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("interface"),
            size: (vertices * std::mem::size_of::<TextVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Construit les quads de l'image et les envoie au GPU.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        overlay: Option<&Overlay>,
        ui: Option<&Ui>,
        size: (u32, u32),
    ) {
        self.count = 0;
        self.models.clear();
        self.splits.clear();
        if overlay.is_none() && ui.is_none() {
            return;
        }
        let (w, h) = (size.0 as f32, size.1 as f32);
        let mut verts = std::mem::take(&mut self.scratch);
        verts.clear();
        let mut b = Builder { verts, w, h };

        if let Some(overlay) = overlay {
            // Taille d'un pixel de police en pixels écran : entière pour garder un texte net.
            let scale = (h / 540.0).floor().max(1.0);
            let glyph = 8.0 * scale;
            let line_h = 10.0 * scale;
            let pad = 2.0 * scale;
            let margin = 4.0 * scale;
            let bg = [0.0, 0.0, 0.0, 0.55];

            // Si les deux colonnes ne tiennent pas côte à côte, la droite passe sous la gauche.
            let col_w = |lines: &[Line]| {
                lines
                    .iter()
                    .map(|l| l.text.chars().count())
                    .max()
                    .unwrap_or(0) as f32
                    * glyph
                    + 2.0 * pad
            };
            let side_by_side = col_w(&overlay.left) + col_w(&overlay.right) + 3.0 * margin <= w;
            let right_first_row = if side_by_side {
                0
            } else {
                overlay.left.len() + 1
            };

            for (lines, right) in [(&overlay.left, false), (&overlay.right, true)] {
                let first_row = if right { right_first_row } else { 0 };
                for (i, line) in lines.iter().enumerate() {
                    if line.text.is_empty() {
                        continue;
                    }
                    let text_w = line.text.chars().count() as f32 * glyph;
                    let x = if right {
                        w - margin - text_w - pad
                    } else {
                        margin + pad
                    };
                    let y = margin + (i + first_row) as f32 * line_h;
                    b.solid(x - pad, y, x + text_w + pad, y + line_h, bg);
                    b.text(x, y + scale, scale, line.color, &line.text, true);
                }
            }
        }

        // Interface : dans l'ordre d'ajout.
        if let Some(ui) = ui {
            for cmd in &ui.cmds {
                match cmd {
                    UiCmd::Rect { x, y, w, h, color } => b.solid(*x, *y, x + w, y + h, *color),
                    UiCmd::Model(verts) => {
                        let start = self.models.len() as u32;
                        self.models.extend(verts.iter().map(|v| {
                            let mut v = *v;
                            v.clip = [
                                v.clip[0] / w * 2.0 - 1.0,
                                1.0 - v.clip[1] / h * 2.0,
                                v.clip[2],
                                1.0,
                            ];
                            v
                        }));
                        self.splits
                            .push((b.verts.len() as u32, start..self.models.len() as u32));
                    }
                    UiCmd::Text {
                        x,
                        y,
                        scale,
                        color,
                        text,
                        shadow,
                    } => b.text(*x, *y, *scale, *color, text, *shadow),
                    UiCmd::Image {
                        name,
                        x,
                        y,
                        w,
                        h,
                        tint,
                    } => {
                        let r = self.sprite_rect(name);
                        b.quad(*x, *y, x + w, y + h, r, *tint);
                    }
                    UiCmd::NineSlice {
                        name,
                        x,
                        y,
                        w,
                        h,
                        border,
                        scale,
                        tint,
                    } => {
                        let r = self.sprite_rect(name);
                        let (bs, bd) = (*border, border * scale);
                        let xs = [r[0], r[0] + bs, r[0] + r[2] - bs, r[0] + r[2]];
                        let ys = [r[1], r[1] + bs, r[1] + r[3] - bs, r[1] + r[3]];
                        let xd = [*x, x + bd, x + w - bd, x + w];
                        let yd = [*y, y + bd, y + h - bd, y + h];
                        for j in 0..3 {
                            for i in 0..3 {
                                let uv = [xs[i], ys[j], xs[i + 1] - xs[i], ys[j + 1] - ys[j]];
                                b.quad(xd[i], yd[j], xd[i + 1], yd[j + 1], uv, *tint);
                            }
                        }
                    }
                    UiCmd::Tiled {
                        name,
                        x,
                        y,
                        w,
                        h,
                        scale,
                        tint,
                    } => {
                        let r = self.sprite_rect(name);
                        let (tw, th) = (r[2] * scale, r[3] * scale);
                        let mut ty = *y;
                        while ty < y + h {
                            let mut tx = *x;
                            let hh = th.min(y + h - ty);
                            while tx < x + w {
                                let ww = tw.min(x + w - tx);
                                let uv = [r[0], r[1], r[2] * ww / tw, r[3] * hh / th];
                                b.quad(tx, ty, tx + ww, ty + hh, uv, *tint);
                                tx += tw;
                            }
                            ty += th;
                        }
                    }
                }
            }
        }

        if b.verts.len() > self.capacity {
            self.capacity = b.verts.len().next_power_of_two();
            self.buffer = Self::create_buffer(device, self.capacity);
            self.gpu_bytes = Self::bytes(self.capacity);
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&b.verts));
        self.count = b.verts.len() as u32;
        self.scratch = b.verts;
    }

    /// Rectangle atlas d'une image (case pleine magenta si elle n'existe pas).
    fn sprite_rect(&self, name: &str) -> [f32; 4] {
        match self.sprites.get(name) {
            Some(r) => r.map(|v| v as f32),
            None => [
                (SOLID % 16 * CELL) as f32 + 2.0,
                (SOLID / 16 * CELL) as f32 + 2.0,
                4.0,
                4.0,
            ],
        }
    }

    pub(crate) fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(0..self.count, 0..1);
    }

    /// Une partie des sommets 2D (entre deux objets 3D).
    pub(crate) fn draw_range<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        range: std::ops::Range<u32>,
    ) {
        let range = range.start.min(self.count)..range.end.min(self.count);
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(range, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_has_glyphs_and_solid_cell() {
        let px = font_bitmap();
        let cell_sum = |code: u32| {
            let (cx, cy) = ((code % 16) * CELL, (code / 16) * CELL);
            (0..CELL)
                .flat_map(|y| (0..CELL).map(move |x| (x, y)))
                .filter(|&(x, y)| px[((cy + y) * 128 + cx + x) as usize] > 0)
                .count()
        };
        assert_eq!(cell_sum(SOLID), 64);
        assert_eq!(cell_sum(' ' as u32), 0);
        assert!(cell_sum('A' as u32) > 10);
        assert!(cell_sum('é' as u32) > 10);
    }
}
