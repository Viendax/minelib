//! Partie graphique intégrée au processeur (iGPU) : mesures au démarrage, en arrière-plan,
//! pour décider à quoi elle peut servir à côté de la carte principale.
//!
//! - puissance de calcul (petit shader de calcul) ;
//! - débit de sa mémoire (copie GPU) et des envois depuis le processeur, comparés à une
//!   copie en mémoire vive par le processeur : sa mémoire est la mémoire vive du PC.

use std::time::Instant;

const MB: u64 = 1024 * 1024;
const SIZE: u64 = 64 * MB;
/// Itérations de calcul par fil (4 opérations flottantes chacune).
const ITERS: u32 = 1024;
const THREADS: u32 = 1 << 20;

#[derive(Clone, Debug)]
pub struct IgpuReport {
    pub name: String,
    pub backend: String,
    /// Milliards d'opérations flottantes par seconde.
    pub gflops: f32,
    /// Copie dans la mémoire de l'iGPU (Go/s).
    pub copy_gbs: f32,
    /// Envoi processeur → iGPU (Go/s).
    pub upload_gbs: f32,
}

/// Cartes vues par wgpu : (nom, type, API).
pub fn adapters() -> Vec<(String, wgpu::DeviceType, wgpu::Backend)> {
    let instance = wgpu::Instance::new(&instance_desc());
    instance
        .enumerate_adapters(instance_desc().backends)
        .into_iter()
        .map(|a| {
            let i = a.get_info();
            (i.name, i.device_type, i.backend)
        })
        .collect()
}

/// Copie mémoire vive → mémoire vive par le processeur (Go/s), pour comparer.
pub fn ram_copy_gbs() -> f32 {
    let src = vec![1u8; SIZE as usize];
    let mut dst = vec![0u8; SIZE as usize];
    dst.copy_from_slice(&src); // première passe : pages réellement allouées
    let t = Instant::now();
    for _ in 0..4 {
        dst.copy_from_slice(&src);
        std::hint::black_box(&dst);
    }
    (4 * SIZE) as f32 / t.elapsed().as_secs_f32() / 1e9
}

/// Mesure la première carte du type demandé (intégrée ou dédiée) ; None s'il n'y en a pas.
pub fn benchmark(kind: wgpu::DeviceType) -> Option<IgpuReport> {
    let instance = wgpu::Instance::new(&instance_desc());
    let adapter = instance
        .enumerate_adapters(instance_desc().backends)
        .into_iter()
        .find(|a| a.get_info().device_type == kind)?;
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("iGPU (mesures)"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits {
            max_buffer_size: SIZE,
            max_storage_buffer_binding_size: (THREADS * 4),
            ..wgpu::Limits::downlevel_defaults()
        },
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    let wait = |device: &wgpu::Device| {
        let _ = device.poll(wgpu::PollType::Wait);
    };
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: SIZE,
            usage,
            mapped_at_creation: false,
        })
    };
    let a = buf(wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST);
    let b = buf(wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST);

    // Envois processeur → iGPU.
    let data = vec![7u8; SIZE as usize];
    queue.write_buffer(&a, 0, &data);
    queue.submit([]);
    wait(&device);
    let t = Instant::now();
    for _ in 0..4 {
        queue.write_buffer(&a, 0, &data);
        queue.submit([]);
    }
    wait(&device);
    let upload_gbs = (4 * SIZE) as f32 / t.elapsed().as_secs_f32() / 1e9;

    // Copies dans la mémoire de l'iGPU.
    let t = Instant::now();
    let mut enc = device.create_command_encoder(&Default::default());
    for _ in 0..8 {
        enc.copy_buffer_to_buffer(&a, 0, &b, 0, SIZE);
    }
    queue.submit([enc.finish()]);
    wait(&device);
    // Lecture + écriture de chaque octet.
    let copy_gbs = (8 * 2 * SIZE) as f32 / t.elapsed().as_secs_f32() / 1e9;

    // Calcul : 2^20 fils × 1024 itérations × 4 opérations.
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iGPU (calcul)"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "@group(0) @binding(0) var<storage, read_write> out: array<f32>;
                @compute @workgroup_size(256)
                fn main(@builtin(global_invocation_id) id: vec3<u32>) {{
                    var a = f32(id.x) * 1e-7;
                    var b = 1.0001;
                    for (var i = 0u; i < {ITERS}u; i++) {{
                        a = a * b + 0.0001;
                        b = b * a - 0.0001;
                    }}
                    out[id.x] = a + b;
                }}"
            )
            .into(),
        ),
    });
    let out = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: THREADS as u64 * 4,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: out.as_entire_binding(),
        }],
    });
    let run = |n: u32| {
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            for _ in 0..n {
                pass.dispatch_workgroups(THREADS / 256, 1, 1);
            }
        }
        queue.submit([enc.finish()]);
        wait(&device);
    };
    run(1); // compilation, mise en route
    let t = Instant::now();
    run(4);
    let gflops =
        (4.0 * THREADS as f64 * ITERS as f64 * 4.0 / t.elapsed().as_secs_f64() / 1e9) as f32;

    Some(IgpuReport {
        name: info.name,
        backend: format!("{:?}", info.backend),
        gflops,
        copy_gbs,
        upload_gbs,
    })
}

/// Sous Windows : DirectX 12 seulement (comme le jeu), sans charger d'autre API en parallèle.
fn instance_desc() -> wgpu::InstanceDescriptor {
    let mut d = wgpu::InstanceDescriptor::from_env_or_default();
    if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
        d.backends = wgpu::Backends::DX12;
    }
    d
}
