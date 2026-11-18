//! Picks how the UI is drawn: wgpu (a GPU, or a software adapter such as WARP), else the CPU
//! renderer. `JWALA_RENDERER=cpu` forces the CPU renderer, `=none` no renderer at all.

use super::painter::{Layer, ShadowJob, TexId};
use super::text::Atlas;
use super::{cpu, gpu};

pub enum Renderer {
    Gpu(gpu::Renderer),
    Cpu(cpu::Renderer),
}

impl Renderer {
    pub fn new(target: gpu::Target, phys: (u32, u32), scale: f32) -> Result<Renderer, String> {
        let mode = std::env::var("JWALA_RENDERER").unwrap_or_default().to_lowercase();
        let gpu_err = if mode == "cpu" {
            "GPU skipped (JWALA_RENDERER=cpu)".to_string()
        } else {
            match gpu::Renderer::new(target, phys, scale) {
                Ok(r) => return Ok(Renderer::Gpu(r)),
                Err(e) => e,
            }
        };
        if mode == "none" {
            return Err(gpu_err);
        }
        eprintln!("Jwala: no GPU renderer ({gpu_err}); drawing on the CPU");
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cpu::Renderer::new(target, phys, scale))) {
            Ok(Ok(r)) => Ok(Renderer::Cpu(r)),
            Ok(Err(e)) => Err(format!("{gpu_err}; CPU renderer: {e}")),
            Err(_) => Err(format!("{gpu_err}; CPU renderer crashed during setup")),
        }
    }

    /// Drawing without a GPU (WARP, llvmpipe or the CPU renderer): frames cost real CPU time.
    pub fn software(&self) -> bool {
        match self {
            Renderer::Gpu(r) => r.software,
            Renderer::Cpu(_) => true,
        }
    }

    pub fn tex(&self, name: &str) -> TexId {
        match self {
            Renderer::Gpu(r) => r.tex(name),
            Renderer::Cpu(r) => r.tex(name),
        }
    }

    pub fn tex_sizes(&self) -> &[(u32, u32)] {
        match self {
            Renderer::Gpu(r) => &r.tex_sizes,
            Renderer::Cpu(r) => &r.tex_sizes,
        }
    }

    pub fn shadow_keys_mut(&mut self) -> &mut Vec<u64> {
        match self {
            Renderer::Gpu(r) => &mut r.shadow_keys,
            Renderer::Cpu(r) => &mut r.shadow_keys,
        }
    }

    pub fn logical_size(&self) -> (f32, f32) {
        match self {
            Renderer::Gpu(r) => r.logical_size(),
            Renderer::Cpu(r) => r.logical_size(),
        }
    }

    pub fn scale(&self) -> f32 {
        match self {
            Renderer::Gpu(r) => r.scale(),
            Renderer::Cpu(r) => r.scale(),
        }
    }

    pub fn resize(&mut self, phys: (u32, u32), scale: f32) {
        match self {
            Renderer::Gpu(r) => r.resize(phys, scale),
            Renderer::Cpu(r) => r.resize(phys, scale),
        }
    }

    pub fn render(&mut self, layer: &Layer, jobs: &[ShadowJob], atlas: &mut Atlas) -> bool {
        match self {
            Renderer::Gpu(r) => r.render(layer, jobs, atlas),
            Renderer::Cpu(r) => r.render(layer, jobs, atlas),
        }
    }
}
