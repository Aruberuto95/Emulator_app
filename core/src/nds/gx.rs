// core/src/nds/gx.rs
//
// NDS geometry-engine command stream decoder (U27). Two input paths, per
// GBATEK: packed command words written to the GXFIFO window (0x04000400-
// 0x0400043F, including via DMA start-timing 7) and direct writes to the
// per-command ports (0x04000440-0x040005C8, one word per parameter).
//
// This stage only DECODES: it maintains the packed/port state machines,
// counts every command, and keeps a bounded trace of fully-decoded commands
// with their parameters. The evidence (which commands SoulSilver's intro
// actually issues) decides what the rasterizer milestone implements.
// ponytail: no FIFO depth/stall model — our GXSTAT HLE reports the FIFO
// permanently empty, so commands can be consumed the moment they arrive.

use std::collections::VecDeque;

/// Parameter word count for a geometry command, or None if the byte is not
/// a known command (a desync symptom on the packed path — counted, skipped).
pub fn param_count(cmd: u8) -> Option<u32> {
    Some(match cmd {
        0x00 => 0,        // NOP (packing filler)
        0x10 => 1,        // MTX_MODE
        0x11 => 0,        // MTX_PUSH
        0x12 => 1,        // MTX_POP
        0x13 => 1,        // MTX_STORE
        0x14 => 1,        // MTX_RESTORE
        0x15 => 0,        // MTX_IDENTITY
        0x16 => 16,       // MTX_LOAD_4x4
        0x17 => 12,       // MTX_LOAD_4x3
        0x18 => 16,       // MTX_MULT_4x4
        0x19 => 12,       // MTX_MULT_4x3
        0x1A => 9,        // MTX_MULT_3x3
        0x1B => 3,        // MTX_SCALE
        0x1C => 3,        // MTX_TRANS
        0x20 => 1,        // COLOR
        0x21 => 1,        // NORMAL
        0x22 => 1,        // TEXCOORD
        0x23 => 2,        // VTX_16
        0x24 => 1,        // VTX_10
        0x25 => 1,        // VTX_XY
        0x26 => 1,        // VTX_XZ
        0x27 => 1,        // VTX_YZ
        0x28 => 1,        // VTX_DIFF
        0x29 => 1,        // POLYGON_ATTR
        0x2A => 1,        // TEXIMAGE_PARAM
        0x2B => 1,        // PLTT_BASE
        0x30 => 1,        // DIF_AMB
        0x31 => 1,        // SPE_EMI
        0x32 => 1,        // LIGHT_VECTOR
        0x33 => 1,        // LIGHT_COLOR
        0x34 => 32,       // SHININESS
        0x40 => 1,        // BEGIN_VTXS
        0x41 => 0,        // END_VTXS
        0x50 => 1,        // SWAP_BUFFERS
        0x60 => 1,        // VIEWPORT
        0x70 => 3,        // BOX_TEST
        0x71 => 2,        // POS_TEST
        0x72 => 1,        // VEC_TEST
        _ => return None,
    })
}

pub fn cmd_name(cmd: u8) -> &'static str {
    match cmd {
        0x00 => "NOP",
        0x10 => "MTX_MODE",
        0x11 => "MTX_PUSH",
        0x12 => "MTX_POP",
        0x13 => "MTX_STORE",
        0x14 => "MTX_RESTORE",
        0x15 => "MTX_IDENTITY",
        0x16 => "MTX_LOAD_4x4",
        0x17 => "MTX_LOAD_4x3",
        0x18 => "MTX_MULT_4x4",
        0x19 => "MTX_MULT_4x3",
        0x1A => "MTX_MULT_3x3",
        0x1B => "MTX_SCALE",
        0x1C => "MTX_TRANS",
        0x20 => "COLOR",
        0x21 => "NORMAL",
        0x22 => "TEXCOORD",
        0x23 => "VTX_16",
        0x24 => "VTX_10",
        0x25 => "VTX_XY",
        0x26 => "VTX_XZ",
        0x27 => "VTX_YZ",
        0x28 => "VTX_DIFF",
        0x29 => "POLYGON_ATTR",
        0x2A => "TEXIMAGE_PARAM",
        0x2B => "PLTT_BASE",
        0x30 => "DIF_AMB",
        0x31 => "SPE_EMI",
        0x32 => "LIGHT_VECTOR",
        0x33 => "LIGHT_COLOR",
        0x34 => "SHININESS",
        0x40 => "BEGIN_VTXS",
        0x41 => "END_VTXS",
        0x50 => "SWAP_BUFFERS",
        0x60 => "VIEWPORT",
        0x70 => "BOX_TEST",
        0x71 => "POS_TEST",
        0x72 => "VEC_TEST",
        _ => "?",
    }
}

/// Bound on the decoded-command trace kept for evidence dumps.
const TRACE_CAP: usize = 768;

/// Triangles accepted per frame before overflow-dropping (evidence counter
/// keeps the drop visible). SoulSilver's intro peaks well below this.
const TRI_CAP: usize = 16384;

/// 4x4 matrix, row-major; vertices are ROW vectors (v' = v * M), so
/// MTX_MULT's "C = M * C" composes child-transform-first like hardware.
type Mtx = [[f32; 4]; 4];

const IDENTITY: Mtx = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn mtx_mul(a: &Mtx, b: &Mtx) -> Mtx {
    let mut out = [[0.0f32; 4]; 4];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    out
}

fn fx32(p: u32) -> f32 {
    p as i32 as f32 / 4096.0
}

fn fx16(p: u16) -> f32 {
    p as i16 as f32 / 4096.0
}

/// Sign-extend a 10-bit field and scale as a 1.3.6 vertex component
/// (VTX_10: value << 6 is the fx16).
fn fx10(p: u32) -> f32 {
    let v = ((p & 0x3FF) as i32) << 22 >> 22;
    (v << 6) as f32 / 4096.0
}

/// Sign-extend a 10-bit field as a 1.0.9 direction component (/512) —
/// NORMAL and LIGHT_VECTOR encode xyz this way.
fn s10n(w: u32, sh: u32) -> f32 {
    ((((w >> sh) & 0x3FF) as i32) << 22 >> 22) as f32 / 512.0
}

/// RGB555 to per-channel f32 in 0..31.
fn rgb5(c: u16) -> [f32; 3] {
    [(c & 0x1F) as f32, ((c >> 5) & 0x1F) as f32, ((c >> 10) & 0x1F) as f32]
}

/// A clip-space vertex during near-plane clipping: homogeneous position,
/// per-vertex color as 5-bit-scale f32 (for gouraud lerp), and texel UVs.
#[derive(Clone, Copy)]
struct ClipV {
    pos: [f32; 4],
    col: [f32; 3],
    uv: [f32; 2],
}

/// Smallest positive w a vertex may keep — anything closer is behind the near
/// plane and gets clipped. Keeps 1/w bounded so the projection stays sane.
const W_NEAR: f32 = 1.0 / 4096.0;

fn lerp_clipv(a: &ClipV, b: &ClipV, t: f32) -> ClipV {
    let mut pos = [0.0f32; 4];
    let mut col = [0.0f32; 3];
    let mut uv = [0.0f32; 2];
    for i in 0..4 {
        pos[i] = a.pos[i] + (b.pos[i] - a.pos[i]) * t;
    }
    for i in 0..3 {
        col[i] = a.col[i] + (b.col[i] - a.col[i]) * t;
    }
    for i in 0..2 {
        uv[i] = a.uv[i] + (b.uv[i] - a.uv[i]) * t;
    }
    ClipV { pos, col, uv }
}

/// Clip a triangle against the single near plane (w >= W_NEAR), fanning the
/// resulting 3- or 4-vertex polygon back into triangles. Returns 0, 1 or 2
/// triangles. Replaces the whole-triangle reject that dropped any primitive
/// with a vertex behind the camera — which erased near objects (overworld
/// furniture/characters) while distant walls survived (U34).
fn clip_near_tri(v: [ClipV; 3]) -> ([[ClipV; 3]; 2], usize) {
    let mut out = [[v[0]; 3]; 2];
    // Build the clipped polygon (Sutherland-Hodgman, one plane).
    let mut poly: Vec<ClipV> = Vec::with_capacity(4);
    for i in 0..3 {
        let a = v[i];
        let b = v[(i + 1) % 3];
        let (ain, bin) = (a.pos[3] >= W_NEAR, b.pos[3] >= W_NEAR);
        if ain {
            poly.push(a);
        }
        if ain != bin {
            let t = (W_NEAR - a.pos[3]) / (b.pos[3] - a.pos[3]);
            poly.push(lerp_clipv(&a, &b, t));
        }
    }
    match poly.len() {
        3 => {
            out[0] = [poly[0], poly[1], poly[2]];
            (out, 1)
        }
        4 => {
            out[0] = [poly[0], poly[1], poly[2]];
            out[1] = [poly[0], poly[2], poly[3]];
            (out, 2)
        }
        _ => (out, 0),
    }
}

/// One clip-space triangle awaiting rasterization: xyzw per vertex, BGR555
/// color per vertex (gouraud), texel-unit UVs, and the texture state
/// (TEXIMAGE_PARAM / PLTT_BASE) captured when it was emitted.
struct Tri {
    v: [[f32; 4]; 3],
    col: [u16; 3],
    uv: [[f32; 2]; 3],
    tex: u32,
    pal: u32,
    /// POLYGON_ATTR bits 16-20 latched at BEGIN_VTXS; 0 (hardware wireframe)
    /// renders opaque. ponytail: wireframe mode unimplemented.
    alpha: u16,
    /// The unmapped POLYGON_ATTR alpha field (0 = hardware wireframe).
    raw_alpha: u16,
}

/// Fetch one texel as (BGR555 color, alpha 0-31), or None for a fully
/// transparent texel. Implements exactly the five formats SoulSilver was
/// measured to use (A3I5, pal4, pal16, pal256, A5I3); 4x4-compressed and
/// direct never appear. A3I5/A5I3 carry per-texel alpha — the title's water
/// caustics overlay blends through it (U31).
fn fetch_texel(
    vram: &crate::nds::mmu::VramManager,
    tex: u32,
    pal_base: u32,
    x: i32,
    y: i32,
) -> Option<(u16, u16)> {
    let w = 8i32 << ((tex >> 20) & 7);
    let h = 8i32 << ((tex >> 23) & 7);
    let wrap = |c: i32, n: i32, repeat: bool, flip: bool| -> i32 {
        if repeat {
            if flip {
                let m = c & (2 * n - 1);
                if m >= n { 2 * n - 1 - m } else { m }
            } else {
                c & (n - 1)
            }
        } else {
            c.clamp(0, n - 1)
        }
    };
    let tx = wrap(x, w, tex & (1 << 16) != 0, tex & (1 << 18) != 0) as u32;
    let ty = wrap(y, h, tex & (1 << 17) != 0, tex & (1 << 19) != 0) as u32;
    let base = (tex & 0xFFFF) * 8;
    let fmt = (tex >> 26) & 7;
    let color0_clear = tex & (1 << 29) != 0;
    let (idx, alpha): (u32, u16) = match fmt {
        1 => {
            // A3I5: 5-bit palette index + 3-bit alpha (scaled to 0-31).
            let b = vram.read_tex_image(base + ty * w as u32 + tx);
            let a3 = (b >> 5) as u16;
            ((b & 0x1F) as u32, a3 * 4 + a3 / 2)
        }
        2 => {
            // pal4: 2 bits per texel.
            let b = vram.read_tex_image(base + (ty * w as u32 + tx) / 4);
            let i = ((b >> ((tx & 3) * 2)) & 3) as u32;
            if i == 0 && color0_clear {
                return None;
            }
            (i, 31)
        }
        3 => {
            // pal16: 4 bits per texel.
            let b = vram.read_tex_image(base + (ty * w as u32 + tx) / 2);
            let i = ((b >> ((tx & 1) * 4)) & 0xF) as u32;
            if i == 0 && color0_clear {
                return None;
            }
            (i, 31)
        }
        4 => {
            let i = vram.read_tex_image(base + ty * w as u32 + tx) as u32;
            if i == 0 && color0_clear {
                return None;
            }
            (i, 31)
        }
        6 => {
            // A5I3: 3-bit palette index + 5-bit alpha.
            let b = vram.read_tex_image(base + ty * w as u32 + tx);
            ((b & 7) as u32, (b >> 3) as u16)
        }
        _ => return None, // 0 = untextured (caller skips), 5/7 unmeasured
    };
    if alpha == 0 {
        return None;
    }
    // pal4 palettes step in 8-byte units, every other format in 16-byte.
    let pal_addr = if fmt == 2 { pal_base * 8 } else { pal_base * 16 } + idx * 2;
    let lo = vram.read_tex_pal(pal_addr) as u16;
    let hi = vram.read_tex_pal(pal_addr + 1) as u16;
    Some(((hi << 8 | lo) & 0x7FFF, alpha))
}

/// Minimal NDS geometry engine + flat rasterizer (U27 milestone 1, lighting
/// added in U30). Scope is exactly the command set the intro/title was
/// measured to issue: matrix stack ops, all six vertex forms, COLOR, vertex
/// lighting (NORMAL/DIF_AMB/SPE_EMI/LIGHT_VECTOR/LIGHT_COLOR with
/// POLYGON_ATTR light enables), BEGIN/END for the four primitive types,
/// SWAP_BUFFERS, VIEWPORT, CLEAR_COLOR fill.
/// ponytail: f32 math instead of the hardware's 20.12 fixed point, whole-
/// triangle rejection instead of near-plane clipping, affine (not
/// perspective-correct) z interpolation — upgrade any of these when a scene
/// shows the artifact. Textures are milestone 2 (measured formats:
/// A3I5/pal4/pal16/pal256/A5I3); specular/shininess skipped until a shot
/// demands it.
pub struct Gx3d {
    mtx_mode: u32,
    proj: Mtx,
    pos: Mtx,
    vec: Mtx,
    tex_mtx: Mtx,
    proj_stack: Mtx,
    pos_stack: [Mtx; 32],
    vec_stack: [Mtx; 32],
    tex_stack: Mtx,
    stack_ptr: usize,
    color: u16,
    last_vtx: [f32; 3],
    prim: u32,
    verts: Vec<([f32; 4], u16, [f32; 2])>,
    tris: Vec<Tri>,
    /// Current texture coordinate in texel units (TEXCOORD is 1/16 texel).
    /// ponytail: texgen modes 2/3 (normal/vertex source) pass through
    /// untransformed — upgrade if an env-mapped surface shows static UVs.
    cur_uv: [f32; 2],
    teximage: u32,
    pltt_base: u32,
    /// SWAP_BUFFERS defers to the owner (`NdsMmu::flush_gx_swap`), which can
    /// lend the texture VRAM the rasterizer samples.
    pub swap_pending: bool,
    /// pos*proj, refreshed lazily — vertices arrive by the millions and the
    /// matrices change far less often.
    clip: Mtx,
    clip_dirty: bool,
    zbuf: Vec<f32>,
    pub tris_dropped: u32,
    pub max_tris_per_frame: usize,
    /// Triangle count of the most recently swapped frame — per-scene evidence.
    pub last_frame_tris: usize,
    /// U34 per-frame raster outcome counters (evidence): triangles rejected
    /// whole because a vertex is behind the near plane, and triangles that
    /// covered zero pixels (fully off-screen / degenerate).
    pub last_near_rejected: usize,
    pub last_zero_px: usize,
    /// U34 character-hunt: textured triangles rejected for a degenerate
    /// (~zero-area) screen projection, and textured triangles that covered
    /// zero pixels — a collapsed or fully-clipped billboard (the character).
    pub last_tex_degenerate: usize,
    pub last_tex_zero_px: usize,
    pub swap_count: u32,
    viewport: (u32, u32, u32, u32),
    // Lighting state (U30): materials + up to 4 directional lights, all in
    // vector-matrix space (LIGHT_VECTOR transforms at write, like hardware).
    dif_amb: u32,
    spe_emi: u32,
    light_vec: [[f32; 3]; 4],
    light_color: [u16; 4],
    poly_attr: u32,
    /// POLYGON_ATTR latches at BEGIN_VTXS (GBATEK) — light enables bits 0-3.
    poly_attr_active: u32,
    /// CLEAR_COLOR (0x04000350) as a framebuffer fill value; 0 = transparent
    /// (the 2D backdrop shows through).
    pub clear_px: u16,
    /// Unique (TEXIMAGE_PARAM, PLTT_BASE) pairs seen at BEGIN_VTXS — bounded
    /// evidence for offline texture decoding.
    pub tex_pairs: Vec<(u32, u32)>,
    /// Env-gated raster census (U33): per (teximage, pltt) — opaque pixels
    /// drawn vs texels skipped as transparent. Names the hole-puncher.
    pub tex_stats_on: bool,
    pub tex_stats: Vec<(u32, u32, u32, u32)>,
    /// GX_NO_TEXGEN=1 diagnostic: bypass the texgen-1 UV transform (A/B
    /// experiment for the granular-overlay artifact).
    texgen_off: bool,
    /// GX_FLAT_LIGHT=1 diagnostic: NORMAL sets plain diffuse (no lights).
    flat_light: bool,
    /// SWAP_BUFFERS bit 1: depth compare on W (linear) instead of Z.
    wbuffer: bool,
    /// Rasterized output, presented by the PPU wherever engine-A BG0 is in
    /// 3D mode. bit15 set = opaque pixel; 0 = transparent (backdrop shows).
    pub fb: Vec<u16>,
    /// W1 evidence: POLYGON_ATTR fields the rasterizer currently ignores.
    /// `attr_mode` counts bits 4-5 (0 modulation, 1 decal, 2 toon/highlight,
    /// 3 shadow volume) and `attr_cull` counts bits 6-7 (0 draws nothing,
    /// 1 front only, 2 back only, 3 both). A nonzero `attr_mode[3]` means
    /// shadow volumes are being rasterized as ordinary opaque geometry, which
    /// is the shape of a large wrong patch; nonzero `attr_cull[1]`/`[2]` means
    /// the missing back-face cull is painting interior faces over exterior.
    pub attr_mode: [u32; 4],
    pub attr_cull: [u32; 4],
    /// Triangles removed by back-face culling this run, and the parity of the
    /// triangle strip in flight (hardware alternates winding).
    pub last_culled: u32,
    strip_odd: bool,
    /// GX_NO_CULL=1 diagnostic: draw every face, the pre-culling behaviour.
    cull_off: bool,
    /// Triangles submitted per texture format, indexed by TEXIMAGE_PARAM bits
    /// 26-28. Formats 5 (4x4-compressed) and 7 (direct colour) are decoded as
    /// fully transparent, so a nonzero count here is a hole-punching polygon.
    pub fmt_tris: [u32; 8],
}

impl Default for Gx3d {
    fn default() -> Self {
        Self {
            mtx_mode: 0,
            proj: IDENTITY,
            pos: IDENTITY,
            vec: IDENTITY,
            tex_mtx: IDENTITY,
            proj_stack: IDENTITY,
            pos_stack: [IDENTITY; 32],
            vec_stack: [IDENTITY; 32],
            tex_stack: IDENTITY,
            stack_ptr: 0,
            color: 0x7FFF,
            last_vtx: [0.0; 3],
            prim: 0,
            verts: Vec::new(),
            tris: Vec::new(),
            cur_uv: [0.0; 2],
            teximage: 0,
            pltt_base: 0,
            swap_pending: false,
            clip: IDENTITY,
            clip_dirty: false,
            zbuf: vec![f32::INFINITY; 256 * 192],
            tris_dropped: 0,
            max_tris_per_frame: 0,
            last_frame_tris: 0,
            last_near_rejected: 0,
            last_zero_px: 0,
            last_tex_degenerate: 0,
            last_tex_zero_px: 0,
            swap_count: 0,
            viewport: (0, 0, 255, 191),
            dif_amb: 0,
            spe_emi: 0,
            light_vec: [[0.0; 3]; 4],
            light_color: [0; 4],
            poly_attr: 0,
            poly_attr_active: 0,
            clear_px: 0,
            tex_pairs: Vec::new(),
            tex_stats_on: false,
            tex_stats: Vec::new(),
            texgen_off: std::env::var("GX_NO_TEXGEN").is_ok(),
            flat_light: std::env::var("GX_FLAT_LIGHT").is_ok(),
            wbuffer: false,
            fb: vec![0; 256 * 192],
            attr_mode: [0; 4],
            attr_cull: [0; 4],
            last_culled: 0,
            strip_odd: false,
            cull_off: std::env::var("GX_NO_CULL").unwrap_or_default() == "1",
            fmt_tris: [0; 8],
        }
    }
}

impl Gx3d {
    /// Matrix the mode targets for loads/mults. Mode 2 updates position and
    /// vector together on hardware; mode 3 is the TEXTURE matrix — U31 bug:
    /// it used to alias onto the vector matrix, so every texture-matrix load
    /// at the title clobbered the lighting space.
    fn cur(&mut self) -> &mut Mtx {
        self.clip_dirty = true;
        match self.mtx_mode {
            0 => &mut self.proj,
            3 => &mut self.tex_mtx,
            _ => &mut self.pos,
        }
    }

    fn mult(&mut self, m: Mtx) {
        let cur = self.cur();
        *cur = mtx_mul(&m, cur);
        if self.mtx_mode == 2 {
            self.vec = mtx_mul(&m, &self.vec);
        }
    }

    /// Direction × the vector matrix's rotation part (row-vector convention,
    /// no translation) — normals and light vectors live in that space.
    fn vec_dir(&self, v: [f32; 3]) -> [f32; 3] {
        let mut out = [0.0f32; 3];
        for (c, o) in out.iter_mut().enumerate() {
            *o = (0..3).map(|r| v[r] * self.vec[r][c]).sum();
        }
        out
    }

    fn exec(&mut self, cmd: u8, p: &[u32]) {
        match cmd {
            0x10 => self.mtx_mode = p.first().copied().unwrap_or(0) & 3,
            0x11 => {
                // MTX_PUSH (projection and texture stacks are single-slot)
                if self.mtx_mode == 0 {
                    self.proj_stack = self.proj;
                } else if self.mtx_mode == 3 {
                    self.tex_stack = self.tex_mtx;
                } else {
                    self.pos_stack[self.stack_ptr & 31] = self.pos;
                    self.vec_stack[self.stack_ptr & 31] = self.vec;
                    self.stack_ptr = (self.stack_ptr + 1).min(63);
                }
            }
            0x12 => {
                // MTX_POP: signed 6-bit stack offset
                self.clip_dirty = true;
                if self.mtx_mode == 0 {
                    self.proj = self.proj_stack;
                } else if self.mtx_mode == 3 {
                    self.tex_mtx = self.tex_stack;
                } else {
                    let n = ((p.first().copied().unwrap_or(1) & 0x3F) as i32) << 26 >> 26;
                    self.stack_ptr = (self.stack_ptr as i32 - n).clamp(0, 63) as usize;
                    self.pos = self.pos_stack[self.stack_ptr & 31];
                    self.vec = self.vec_stack[self.stack_ptr & 31];
                }
            }
            0x13 => {
                // MTX_STORE to slot
                let s = (p.first().copied().unwrap_or(0) & 0x1F) as usize;
                if self.mtx_mode == 0 {
                    self.proj_stack = self.proj;
                } else if self.mtx_mode == 3 {
                    self.tex_stack = self.tex_mtx;
                } else {
                    self.pos_stack[s] = self.pos;
                    self.vec_stack[s] = self.vec;
                }
            }
            0x14 => {
                // MTX_RESTORE from slot
                self.clip_dirty = true;
                let s = (p.first().copied().unwrap_or(0) & 0x1F) as usize;
                if self.mtx_mode == 0 {
                    self.proj = self.proj_stack;
                } else if self.mtx_mode == 3 {
                    self.tex_mtx = self.tex_stack;
                } else {
                    self.pos = self.pos_stack[s];
                    self.vec = self.vec_stack[s];
                }
            }
            0x15 => *self.cur() = IDENTITY,
            0x16 | 0x18 => {
                // 4x4 load/mult
                let mut m = IDENTITY;
                for (i, &w) in p.iter().take(16).enumerate() {
                    m[i / 4][i % 4] = fx32(w);
                }
                if cmd == 0x16 {
                    *self.cur() = m;
                    if self.mtx_mode == 2 {
                        self.vec = m;
                    }
                } else {
                    self.mult(m);
                }
            }
            0x17 | 0x19 => {
                // 4x3 load/mult (rows of 3, last column 0,0,0,1)
                let mut m = IDENTITY;
                for (i, &w) in p.iter().take(12).enumerate() {
                    m[i / 3][i % 3] = fx32(w);
                }
                if cmd == 0x17 {
                    *self.cur() = m;
                    if self.mtx_mode == 2 {
                        self.vec = m;
                    }
                } else {
                    self.mult(m);
                }
            }
            0x1A => {
                // 3x3 mult
                let mut m = IDENTITY;
                for (i, &w) in p.iter().take(9).enumerate() {
                    m[i / 3][i % 3] = fx32(w);
                }
                self.mult(m);
            }
            0x1B => {
                // MTX_SCALE (never touches the vector matrix)
                let mut m = IDENTITY;
                for i in 0..3 {
                    m[i][i] = fx32(p.get(i).copied().unwrap_or(0x1000));
                }
                let mode = self.mtx_mode;
                self.mtx_mode = if mode == 2 { 1 } else { mode };
                self.mult(m);
                self.mtx_mode = mode;
            }
            0x1C => {
                // MTX_TRANS
                let mut m = IDENTITY;
                for i in 0..3 {
                    m[3][i] = fx32(p.get(i).copied().unwrap_or(0));
                }
                self.mult(m);
            }
            0x20 => self.color = (p.first().copied().unwrap_or(0) as u16) & 0x7FFF,
            0x21 => {
                // NORMAL: evaluate vertex lighting (GBATEK) — color =
                // emission + Σ enabled lights (ambient·light +
                // diffuse·light·max(0, -N·L) + specular·light·spec), 5-bit
                // channels clamped. Specular (U32): half-vector between the
                // light and the line of sight (0,0,-1); level = max(0,
                // 2·(N·Ĥ)²−1) — the title Lugia's silver sheen needs it.
                // ponytail: the SHININESS remap table is treated as identity.
                let w = p.first().copied().unwrap_or(0);
                if self.flat_light {
                    // GX_FLAT_LIGHT=1 diagnostic: bypass the light equation.
                    self.color = self.dif_amb as u16 & 0x7FFF;
                    return;
                }
                let n = self.vec_dir([s10n(w, 0), s10n(w, 10), s10n(w, 20)]);
                let dif = rgb5(self.dif_amb as u16 & 0x7FFF);
                let amb = rgb5((self.dif_amb >> 16) as u16 & 0x7FFF);
                let spe = rgb5(self.spe_emi as u16 & 0x7FFF);
                let mut acc = rgb5((self.spe_emi >> 16) as u16 & 0x7FFF);
                for l in 0..4 {
                    if self.poly_attr_active & (1 << l) == 0 {
                        continue;
                    }
                    let lc = rgb5(self.light_color[l]);
                    let lv = self.light_vec[l];
                    let dl = (-(lv[0] * n[0] + lv[1] * n[1] + lv[2] * n[2])).max(0.0);
                    let h = [lv[0], lv[1], lv[2] - 1.0];
                    let hm = (h[0] * h[0] + h[1] * h[1] + h[2] * h[2]).sqrt().max(1e-6);
                    let hd = (-(h[0] * n[0] + h[1] * n[1] + h[2] * n[2]) / hm).max(0.0);
                    let sl = (2.0 * hd * hd - 1.0).max(0.0);
                    for c in 0..3 {
                        acc[c] += (dif[c] * dl + amb[c] + spe[c] * sl) * lc[c] / 31.0;
                    }
                }
                let q = |v: f32| v.round().clamp(0.0, 31.0) as u16;
                self.color = q(acc[0]) | (q(acc[1]) << 5) | (q(acc[2]) << 10);
            }
            0x23 => {
                let w0 = p.first().copied().unwrap_or(0);
                let w1 = p.get(1).copied().unwrap_or(0);
                self.vertex(fx16(w0 as u16), fx16((w0 >> 16) as u16), fx16(w1 as u16));
            }
            0x24 => {
                let w = p.first().copied().unwrap_or(0);
                self.vertex(fx10(w), fx10(w >> 10), fx10(w >> 20));
            }
            0x25 => {
                let w = p.first().copied().unwrap_or(0);
                self.vertex(fx16(w as u16), fx16((w >> 16) as u16), self.last_vtx[2]);
            }
            0x26 => {
                let w = p.first().copied().unwrap_or(0);
                self.vertex(fx16(w as u16), self.last_vtx[1], fx16((w >> 16) as u16));
            }
            0x27 => {
                let w = p.first().copied().unwrap_or(0);
                self.vertex(self.last_vtx[0], fx16(w as u16), fx16((w >> 16) as u16));
            }
            0x28 => {
                // VTX_DIFF: three signed 10-bit fx deltas
                let w = p.first().copied().unwrap_or(0);
                let d = |sh: u32| (((w >> sh) & 0x3FF) as i32) << 22 >> 22;
                self.vertex(
                    self.last_vtx[0] + d(0) as f32 / 4096.0,
                    self.last_vtx[1] + d(10) as f32 / 4096.0,
                    self.last_vtx[2] + d(20) as f32 / 4096.0,
                );
            }
            0x22 => {
                // TEXCOORD: signed 1.11.4 s/t -> texel units. Texgen mode 1
                // (TexCoord source) transforms through the texture matrix at
                // write time — the title scrolls its water caustics this way.
                let w = p.first().copied().unwrap_or(0);
                let s = (w as u16 as i16) as f32 / 16.0;
                let t = ((w >> 16) as u16 as i16) as f32 / 16.0;
                self.cur_uv = if (self.teximage >> 30) & 3 == 1 && !self.texgen_off {
                    let m = &self.tex_mtx;
                    [
                        s * m[0][0] + t * m[1][0] + (m[2][0] + m[3][0]) / 16.0,
                        s * m[0][1] + t * m[1][1] + (m[2][1] + m[3][1]) / 16.0,
                    ]
                } else {
                    // ponytail: texgen modes 2/3 (normal/vertex source) pass
                    // through — the title trace shows only modes 0/1.
                    [s, t]
                };
            }
            0x29 => self.poly_attr = p.first().copied().unwrap_or(0),
            0x2A => self.teximage = p.first().copied().unwrap_or(0),
            0x2B => self.pltt_base = p.first().copied().unwrap_or(0) & 0x1FFF,
            0x30 => {
                // DIF_AMB: bit15 = also set the vertex color to the diffuse.
                let w = p.first().copied().unwrap_or(0);
                self.dif_amb = w;
                if w & 0x8000 != 0 {
                    self.color = w as u16 & 0x7FFF;
                }
            }
            0x31 => self.spe_emi = p.first().copied().unwrap_or(0),
            0x32 => {
                // LIGHT_VECTOR: bits30-31 select the light; direction is
                // transformed by the current vector matrix at write time.
                let w = p.first().copied().unwrap_or(0);
                self.light_vec[((w >> 30) & 3) as usize] =
                    self.vec_dir([s10n(w, 0), s10n(w, 10), s10n(w, 20)]);
            }
            0x33 => {
                let w = p.first().copied().unwrap_or(0);
                self.light_color[((w >> 30) & 3) as usize] = w as u16 & 0x7FFF;
            }
            0x40 => {
                self.prim = p.first().copied().unwrap_or(0) & 3;
                self.poly_attr_active = self.poly_attr;
                // Evidence collector (U32): unique texture/palette pairs per
                // run — names every texture the scene draws with, bounded.
                if self.teximage != 0
                    && self.tex_pairs.len() < 64
                    && !self.tex_pairs.contains(&(self.teximage, self.pltt_base))
                {
                    self.tex_pairs.push((self.teximage, self.pltt_base));
                }
                self.verts.clear();
                self.strip_odd = false;
            }
            0x50 => {
                // Bit 1 of the SWAP_BUFFERS parameter selects W-buffering.
                // SoulSilver's title runs with it set — z/w is nearly constant
                // across its scenes, so Z-buffering there degenerates into
                // per-pixel float coin-flips (scattered holes through every
                // layer, the "missing pixels" defect).
                self.wbuffer = p.first().copied().unwrap_or(0) & 2 != 0;
                self.swap_pending = true;
            }
            0x60 => {
                let w = p.first().copied().unwrap_or(0);
                self.viewport = (w & 0xFF, (w >> 8) & 0xFF, (w >> 16) & 0xFF, (w >> 24) & 0xFF);
            }
            // TEXCOORD, TEXIMAGE_PARAM, PLTT_BASE, SHININESS, END_VTXS,
            // tests: consumed without effect (textures = milestone 2).
            _ => {}
        }
    }

    fn vertex(&mut self, x: f32, y: f32, z: f32) {
        self.last_vtx = [x, y, z];
        if self.clip_dirty {
            self.clip = mtx_mul(&self.pos, &self.proj);
            self.clip_dirty = false;
        }
        let v = [x, y, z, 1.0];
        let mut out = [0.0f32; 4];
        for (c, o) in out.iter_mut().enumerate() {
            *o = (0..4).map(|r| v[r] * self.clip[r][c]).sum();
        }
        self.verts.push((out, self.color, self.cur_uv));
        // Emit triangles per primitive type as soon as enough vertices exist.
        let n = self.verts.len();
        match self.prim {
            0 if n == 3 => {
                self.emit([0, 1, 2]);
                self.verts.clear();
            }
            1 if n == 4 => {
                self.emit([0, 1, 2]);
                self.emit([0, 2, 3]);
                self.verts.clear();
            }
            2 if n >= 3 => {
                // Triangle strip. Hardware alternates winding every other
                // triangle, so the second, fourth, ... must have two vertices
                // swapped or they face the opposite way. This is invisible
                // without culling and mandatory with it. Parity is tracked on a
                // counter rather than derived from `n`, because the drain below
                // resets `n` and would flip the sequence mid-strip.
                if self.strip_odd {
                    self.emit([n - 2, n - 3, n - 1]);
                } else {
                    self.emit([n - 3, n - 2, n - 1]);
                }
                self.strip_odd = !self.strip_odd;
                if n > 128 {
                    self.verts.drain(..n - 2);
                }
            }
            3 if n >= 4 && n % 2 == 0 => {
                // Quad strip: (0,1,3,2) per pair.
                self.emit([n - 4, n - 3, n - 1]);
                self.emit([n - 4, n - 1, n - 2]);
                if n > 128 {
                    self.verts.drain(..n - 2);
                }
            }
            _ => {}
        }
    }

    fn emit(&mut self, idx: [usize; 3]) {
        if self.tris.len() >= TRI_CAP {
            self.tris_dropped += 1;
            return;
        }
        // Evidence only: record the POLYGON_ATTR fields the rasterizer does not
        // yet consume, and which texture format this triangle carries.
        self.attr_mode[((self.poly_attr_active >> 4) & 3) as usize] += 1;
        self.attr_cull[((self.poly_attr_active >> 6) & 3) as usize] += 1;
        self.fmt_tris[((self.teximage >> 26) & 7) as usize] += 1;
        // Back-face culling. POLYGON_ATTR bit 6 renders back faces and bit 7
        // renders front faces, so 0 draws nothing at all and 3 draws both.
        // Measured on SoulSilver: 823054 triangles ask for front-only and
        // 62470 for both, so drawing every face buries each model's front
        // surface under its own interior.
        //
        // Facing is decided in CLIP space, before near-plane clipping, because
        // the screen-space signed area is only computed after the viewport's Y
        // flip and would report the opposite sign.
        let cull = (self.poly_attr_active >> 6) & 3;
        if !self.cull_off {
            let [pa, pb, pc] = idx.map(|i| self.verts[i].0);
            // Signed volume of the clip-space triangle against the eye.
            let e1 = [pb[0] - pa[0], pb[1] - pa[1], pb[3] - pa[3]];
            let e2 = [pc[0] - pa[0], pc[1] - pa[1], pc[3] - pa[3]];
            let n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            // Sign determined by measurement, not derivation: the opposite
            // convention culled the visible surfaces instead of the hidden
            // ones, collapsing the bedroom scene's opaque coverage from 49152
            // pixels (full screen) to 4259.
            let front = pa[0] * n[0] + pa[1] * n[1] + pa[3] * n[2] > 0.0;
            let draw = match cull {
                0 => false,
                1 => !front,
                2 => front,
                _ => true,
            };
            if !draw {
                self.last_culled += 1;
                return;
            }
        }
        let [a, b, c] = idx.map(|i| &self.verts[i]);
        self.tris.push(Tri {
            v: [a.0, b.0, c.0],
            col: [a.1, b.1, c.1],
            uv: [a.2, b.2, c.2],
            tex: self.teximage,
            pal: self.pltt_base,
            alpha: match (self.poly_attr_active >> 16) & 0x1F {
                0 => 31,
                a => a as u16,
            },
            raw_alpha: ((self.poly_attr_active >> 16) & 0x1F) as u16,
        });
    }

    pub fn swap_buffers(&mut self, vram: &crate::nds::mmu::VramManager) {
        self.swap_pending = false;
        self.swap_count += 1;
        self.max_tris_per_frame = self.max_tris_per_frame.max(self.tris.len());
        let (x1, y1, x2, y2) = self.viewport;
        let vw = (x2.wrapping_sub(x1) & 0xFF) as f32 + 1.0;
        let vh = (y2.wrapping_sub(y1) & 0xFF) as f32 + 1.0;
        let clear = self.clear_px;
        let tris = std::mem::take(&mut self.tris);
        self.last_frame_tris = tris.len();
        let wbuf = self.wbuffer;
        let no_ztest = std::env::var("GX_NO_ZTEST").is_ok();
        let Gx3d { fb, zbuf, tex_stats, tex_stats_on, .. } = self;
        let stats_on = *tex_stats_on;
        let mut bump = |tex: u32, pal: u32, opaque: bool| {
            if let Some(e) = tex_stats.iter_mut().find(|e| e.0 == tex && e.1 == pal) {
                if opaque {
                    e.2 += 1;
                } else {
                    e.3 += 1;
                }
            } else if tex_stats.len() < 32 {
                tex_stats.push((tex, pal, opaque as u32, !opaque as u32));
            }
        };
        fb.iter_mut().for_each(|p| *p = clear);
        zbuf.iter_mut().for_each(|z| *z = f32::INFINITY);
        // Hardware draw order (U33): all OPAQUE polygons render first, then
        // the translucent ones blend over the finished scene. Submission
        // order let early-submitted overlays (A3I5/A5I3, alpha<31) blend
        // against the clear color before the opaque water/scenery existed —
        // the granular dark speckle over the whole title.
        // ponytail: translucent pass stays in submission order (hardware
        // Y-sorts; add if layering artifacts show).
        let translucent = |t: &Tri| {
            let fmt = (t.tex >> 26) & 7;
            fmt == 1 || fmt == 6 || t.alpha < 31
        };
        let mut order: Vec<usize> = (0..tris.len()).filter(|&i| !translucent(&tris[i])).collect();
        order.extend((0..tris.len()).filter(|&i| translucent(&tris[i])));
        // Focused evidence (U33, active only while the census runs): raw
        // per-triangle UVs + w for the title Lugia's A5I3 silhouette.
        let mut focus_logged = 0u32;
        let mut near_rejected = 0usize;
        let mut tex_degenerate = 0usize;
        let mut tex_zero_px = 0usize;
        for &ti in &order {
            let t = &tris[ti];
            if stats_on
                && (t.tex & 0xFFFF) * 8 == 0x0000
                && (t.tex >> 26) & 7 == 3
                && focus_logged < 16
            {
                focus_logged += 1;
                eprintln!(
                    "    FOCUSTRI addr={:#07x} fmt={} alpha={} raw_attr_alpha={} uv=({:.1},{:.1})({:.1},{:.1})({:.1},{:.1})",
                    (t.tex & 0xFFFF) * 8,
                    (t.tex >> 26) & 7,
                    t.alpha,
                    t.raw_alpha,
                    t.uv[0][0], t.uv[0][1], t.uv[1][0], t.uv[1][1], t.uv[2][0], t.uv[2][1],
                );
            }
            let textured = (t.tex >> 26) & 7 != 0;
            let poly_a = t.alpha as f32;
            // Near-plane clip (U34): a triangle with a vertex behind the
            // camera is split at the near plane instead of dropped whole, so
            // near objects (overworld furniture/characters) survive.
            let base = [0usize, 1, 2].map(|i| ClipV {
                pos: t.v[i],
                col: rgb5(t.col[i]),
                uv: t.uv[i],
            });
            if t.v.iter().any(|v| v[3] < W_NEAR) {
                near_rejected += 1;
            }
            let (subtris, nsub) = clip_near_tri(base);
            for sub in subtris.iter().take(nsub) {
            let mut s = [[0.0f32; 4]; 3];
            for (i, v) in sub.iter().enumerate() {
                let inv_w = 1.0 / v.pos[3];
                s[i] = [
                    x1 as f32 + (v.pos[0] * inv_w + 1.0) * 0.5 * vw,
                    // NDS screen y grows downward; NDC +y is up.
                    y1 as f32 + (1.0 - v.pos[1] * inv_w) * 0.5 * vh,
                    v.pos[2] * inv_w,
                    inv_w,
                ];
            }
            let area = (s[1][0] - s[0][0]) * (s[2][1] - s[0][1])
                - (s[2][0] - s[0][0]) * (s[1][1] - s[0][1]);
            if area.abs() < 1e-6 {
                if textured {
                    tex_degenerate += 1;
                    if stats_on && focus_logged < 8 {
                        focus_logged += 1;
                        eprintln!(
                            "    DEGEN clipW=({:.3},{:.3},{:.3}) scr=({:.1},{:.1})({:.1},{:.1})({:.1},{:.1}) fmt={}",
                            sub[0].pos[3], sub[1].pos[3], sub[2].pos[3],
                            s[0][0], s[0][1], s[1][0], s[1][1], s[2][0], s[2][1],
                            (t.tex >> 26) & 7,
                        );
                    }
                }
                continue;
            }
            // Per-vertex 5-bit channels for gouraud interpolation.
            let cols = [sub[0].col, sub[1].col, sub[2].col];
            let uvs = [sub[0].uv, sub[1].uv, sub[2].uv];
            let mut wrote_px = false;
            let min_x = s.iter().map(|v| v[0]).fold(f32::INFINITY, f32::min).max(0.0) as usize;
            let max_x =
                (s.iter().map(|v| v[0]).fold(f32::NEG_INFINITY, f32::max).min(255.0)) as usize;
            let min_y = s.iter().map(|v| v[1]).fold(f32::INFINITY, f32::min).max(0.0) as usize;
            let max_y =
                (s.iter().map(|v| v[1]).fold(f32::NEG_INFINITY, f32::max).min(191.0)) as usize;
            for py in min_y..=max_y.min(191) {
                for px in min_x..=max_x.min(255) {
                    let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                    let w0 = (s[1][0] - s[0][0]) * (fy - s[0][1]) - (fx - s[0][0]) * (s[1][1] - s[0][1]);
                    let w1 = (s[2][0] - s[1][0]) * (fy - s[1][1]) - (fx - s[1][0]) * (s[2][1] - s[1][1]);
                    let w2 = (s[0][0] - s[2][0]) * (fy - s[2][1]) - (fx - s[2][0]) * (s[0][1] - s[2][1]);
                    // Barycentric inside-test with a small tolerance (U33):
                    // exact >=0 edge tests computed independently per triangle
                    // let float rounding disown shared-edge pixels from BOTH
                    // neighbors — single-pixel seam cracks that exposed the 2D
                    // backdrop as a dot grid across every 3D scene. Dividing
                    // by the SIGNED area normalizes both windings; the epsilon
                    // trades cracks for a harmless 1-pixel overlap.
                    let (b0, b1, b2) = (w1 / area, w2 / area, w0 / area);
                    const EDGE_EPS: f32 = 1e-4;
                    if b0 < -EDGE_EPS || b1 < -EDGE_EPS || b2 < -EDGE_EPS {
                        continue;
                    }
                    let iw = b0 * s[0][3] + b1 * s[1][3] + b2 * s[2][3];
                    // Depth per SWAP_BUFFERS bit 1: W (linear, well-spread)
                    // or Z (z/w, which SoulSilver's projections collapse).
                    let z = if wbuf {
                        1.0 / iw
                    } else {
                        b0 * s[0][2] + b1 * s[1][2] + b2 * s[2][2]
                    };
                    let o = py * 256 + px;
                    if z >= zbuf[o] && !no_ztest {
                        continue;
                    }
                    // Gouraud vertex color.
                    let mut r = b0 * cols[0][0] + b1 * cols[1][0] + b2 * cols[2][0];
                    let mut g = b0 * cols[0][1] + b1 * cols[1][1] + b2 * cols[2][1];
                    let mut b = b0 * cols[0][2] + b1 * cols[1][2] + b2 * cols[2][2];
                    let mut alpha = poly_a;
                    if textured {
                        // Perspective-correct UVs (U32): interpolate uv/w and
                        // 1/w, divide per pixel — affine UVs visibly warped
                        // oblique models (the title Lugia's "unloaded"-looking
                        // body). ponytail: vertex colors stay affine.
                        let u = (b0 * uvs[0][0] * s[0][3]
                            + b1 * uvs[1][0] * s[1][3]
                            + b2 * uvs[2][0] * s[2][3])
                            / iw;
                        let v = (b0 * uvs[0][1] * s[0][3]
                            + b1 * uvs[1][1] * s[1][3]
                            + b2 * uvs[2][1] * s[2][3])
                            / iw;
                        match fetch_texel(vram, t.tex, t.pal, u.floor() as i32, v.floor() as i32) {
                            Some((tc, ta)) => {
                                // Modulate texel by vertex color.
                                // ponytail: (t*v)/31 instead of the exact
                                // 6-bit hardware ramp — visually equivalent.
                                let tc5 = rgb5(tc);
                                r = tc5[0] * r / 31.0;
                                g = tc5[1] * g / 31.0;
                                b = tc5[2] * b / 31.0;
                                alpha = alpha * ta as f32 / 31.0;
                                if stats_on {
                                    bump(t.tex, t.pal, true);
                                }
                            }
                            None => {
                                if stats_on {
                                    bump(t.tex, t.pal, false);
                                }
                                continue; // transparent texel
                            }
                        }
                    }
                    let q = |v: f32| (v.round().clamp(0.0, 31.0)) as u16;
                    wrote_px = true;
                    if alpha >= 30.5 {
                        zbuf[o] = z;
                        fb[o] = q(r) | (q(g) << 5) | (q(b) << 10) | 0x8000;
                    } else {
                        // Semi-transparent (texel and/or POLYGON_ATTR alpha):
                        // z-tested blend over whatever is already there.
                        // ponytail: translucent pixels don't own the z-buffer
                        // (bit11 ignored) and blend against the clear color
                        // when the destination is transparent — the 3D layer
                        // can't see the 2D backdrop from here.
                        let dst = if fb[o] & 0x8000 != 0 { fb[o] } else { clear };
                        if dst & 0x8000 != 0 {
                            let d = rgb5(dst & 0x7FFF);
                            let f = alpha / 31.0;
                            r = r * f + d[0] * (1.0 - f);
                            g = g * f + d[1] * (1.0 - f);
                            b = b * f + d[2] * (1.0 - f);
                        }
                        fb[o] = q(r) | (q(g) << 5) | (q(b) << 10) | 0x8000;
                    }
                }
            }
            if textured && !wrote_px {
                tex_zero_px += 1;
            }
            }
        }
        self.last_near_rejected = near_rejected;
        self.last_tex_degenerate = tex_degenerate;
        self.last_tex_zero_px = tex_zero_px;
        self.last_zero_px = 0;
    }
}

pub struct GxDecoder {
    /// Count per command byte (0x00-0x7F).
    pub histo: [u32; 0x80],
    /// Command bytes seen on the packed path that param_count doesn't know —
    /// nonzero means the packed decoder desynced (or a new command exists).
    pub unknown_cmds: u32,
    /// TEXIMAGE_PARAM format field (bits 26-28) histogram — names the texture
    /// formats a future milestone-2 must implement. [0]=none/untextured.
    pub tex_fmt_histo: [u32; 8],
    /// BEGIN_VTXS primitive type (param bits 0-1) histogram.
    pub begin_histo: [u32; 4],
    /// First TRACE_CAP fully-decoded commands (with parameters) recorded
    /// while `trace_on` — the probe arms it at a scene of interest, because
    /// boot floods the stream with thousands of PUSH/IDENTITY/POP init
    /// triples before the first real display list.
    pub trace: Vec<(u8, Vec<u32>)>,
    pub trace_on: bool,
    /// The geometry engine + rasterizer every committed command feeds.
    pub engine: Gx3d,

    // Packed-FIFO state: commands from the current packed word still waiting
    // for parameters, and how many words the front one still needs.
    fifo_cmds: VecDeque<u8>,
    fifo_params_left: u32,
    fifo_params: Vec<u32>,

    // Direct-port state: a multi-parameter port command accumulates across
    // consecutive writes to its port.
    port_cmd: u8,
    port_params_left: u32,
    port_params: Vec<u32>,
}

impl Default for GxDecoder {
    fn default() -> Self {
        Self {
            histo: [0; 0x80],
            unknown_cmds: 0,
            tex_fmt_histo: [0; 8],
            begin_histo: [0; 4],
            trace: Vec::new(),
            trace_on: true,
            engine: Gx3d::default(),
            fifo_cmds: VecDeque::new(),
            fifo_params_left: 0,
            fifo_params: Vec::new(),
            port_cmd: 0,
            port_params_left: 0,
            port_params: Vec::new(),
        }
    }
}

impl GxDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn total(&self) -> u64 {
        self.histo.iter().map(|&c| c as u64).sum()
    }

    fn commit(&mut self, cmd: u8, params: Vec<u32>) {
        self.histo[(cmd & 0x7F) as usize] = self.histo[(cmd & 0x7F) as usize].wrapping_add(1);
        match cmd {
            0x2A => {
                let fmt = ((params.first().copied().unwrap_or(0) >> 26) & 7) as usize;
                self.tex_fmt_histo[fmt] = self.tex_fmt_histo[fmt].wrapping_add(1);
            }
            0x40 => {
                let prim = (params.first().copied().unwrap_or(0) & 3) as usize;
                self.begin_histo[prim] = self.begin_histo[prim].wrapping_add(1);
            }
            _ => {}
        }
        if self.trace_on && self.trace.len() < TRACE_CAP {
            self.trace.push((cmd, params.clone()));
        }
        self.engine.exec(cmd, &params);
    }

    /// After queueing packed commands (or finishing one), commit every
    /// leading zero-parameter command and point fifo_params_left at the
    /// first one that actually needs words.
    fn fifo_advance(&mut self) {
        while let Some(&cmd) = self.fifo_cmds.front() {
            let need = param_count(cmd).unwrap_or(0);
            if need == 0 {
                self.fifo_cmds.pop_front();
                self.commit(cmd, Vec::new());
            } else {
                self.fifo_params_left = need;
                self.fifo_params = Vec::with_capacity(need.min(16) as usize);
                return;
            }
        }
        self.fifo_params_left = 0;
    }

    /// A word written into the GXFIFO window (0x400-0x43F): either the next
    /// parameter of the pending command, or a new packed 4-command word.
    pub fn push_fifo_word(&mut self, val: u32) {
        if self.fifo_params_left > 0 {
            if self.fifo_params.len() < 16 {
                self.fifo_params.push(val);
            }
            self.fifo_params_left -= 1;
            if self.fifo_params_left == 0 {
                let cmd = self.fifo_cmds.pop_front().unwrap_or(0);
                let params = std::mem::take(&mut self.fifo_params);
                self.commit(cmd, params);
                self.fifo_advance();
            }
            return;
        }
        // New packed word: up to 4 command bytes, low byte first. Zero bytes
        // are packing filler (only count a NOP word if the whole word is 0).
        if val == 0 {
            self.commit(0x00, Vec::new());
            return;
        }
        for i in 0..4 {
            let cmd = ((val >> (8 * i)) & 0xFF) as u8;
            if cmd == 0 {
                continue;
            }
            if param_count(cmd).is_none() {
                self.unknown_cmds = self.unknown_cmds.wrapping_add(1);
                continue;
            }
            self.fifo_cmds.push_back(cmd);
        }
        self.fifo_advance();
    }

    /// A word written to a direct command port (cmd = (addr-0x400)/4 for
    /// 0x440-0x5C8). Each write carries one parameter; zero-parameter
    /// commands execute once per write.
    pub fn push_port_word(&mut self, cmd: u8, val: u32) {
        let need = match param_count(cmd) {
            Some(n) => n,
            None => {
                self.unknown_cmds = self.unknown_cmds.wrapping_add(1);
                return;
            }
        };
        if need == 0 {
            self.commit(cmd, Vec::new());
            return;
        }
        if self.port_params_left > 0 && self.port_cmd == cmd {
            if self.port_params.len() < 16 {
                self.port_params.push(val);
            }
            self.port_params_left -= 1;
        } else {
            // A different command starting mid-group aborts the old group
            // (evidence decoder: log nothing, hardware would have stalled).
            self.port_cmd = cmd;
            self.port_params = vec![val];
            self.port_params_left = need - 1;
        }
        if self.port_params_left == 0 {
            let params = std::mem::take(&mut self.port_params);
            self.commit(cmd, params);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nds::mmu::VramManager;

    /// SWAP_BUFFERS defers to the owner in production (NdsMmu lends texture
    /// VRAM); tests flush with an empty VRAM unless they build one.
    fn flush(gx: &mut GxDecoder) {
        gx.engine.swap_buffers(&VramManager::new());
    }

    #[test]
    fn packed_fifo_word_decodes_commands_in_order_with_params() {
        let mut gx = GxDecoder::new();
        // Packed word: [MTX_MODE, MTX_PUSH, MTX_IDENTITY, COLOR] low byte
        // first. Params follow in command order: 1 for MTX_MODE, 0, 0, 1 for
        // COLOR.
        gx.push_fifo_word(0x2015_1110);
        gx.push_fifo_word(2); // MTX_MODE param (projection)
        gx.push_fifo_word(0x7FFF); // COLOR param
        assert_eq!(
            gx.trace,
            vec![
                (0x10, vec![2]),
                (0x11, vec![]),
                (0x15, vec![]),
                (0x20, vec![0x7FFF]),
            ]
        );
        assert_eq!(gx.histo[0x10], 1);
        assert_eq!(gx.histo[0x11], 1);
        assert_eq!(gx.unknown_cmds, 0);
        // Next word starts a fresh packed group: a lone VTX_16 needs 2 words.
        gx.push_fifo_word(0x0000_0023);
        gx.push_fifo_word(0x1234_5678);
        gx.push_fifo_word(0x0000_4321);
        assert_eq!(gx.trace[4], (0x23, vec![0x1234_5678, 0x4321]));
    }

    #[test]
    fn port_writes_accumulate_multiword_params_and_track_teximage_formats() {
        let mut gx = GxDecoder::new();
        // MTX_TRANS via its port: 3 writes = one command.
        for v in [1u32, 2, 3] {
            gx.push_port_word(0x1C, v);
        }
        assert_eq!(gx.trace, vec![(0x1C, vec![1, 2, 3])]);
        // Zero-param port command executes per write.
        gx.push_port_word(0x11, 0xDEAD);
        gx.push_port_word(0x11, 0xBEEF);
        assert_eq!(gx.histo[0x11], 2);
        // TEXIMAGE_PARAM format field lands in the format histogram.
        gx.push_port_word(0x2A, 3 << 26); // format 3 = 16-color palette
        assert_eq!(gx.tex_fmt_histo[3], 1);
        // BEGIN_VTXS primitive histogram.
        gx.push_port_word(0x40, 1); // quads
        assert_eq!(gx.begin_histo[1], 1);
        // Commands committed: MTX_TRANS + 2x MTX_PUSH + TEXIMAGE + BEGIN = 5.
        assert_eq!(gx.total(), 5);
    }

    /// Draw one triangle with a given POLYGON_ATTR cull field and report
    /// whether it reached the framebuffer.
    fn draws_with_cull(cull: u32) -> bool {
        let mut gx = GxDecoder::new();
        gx.push_port_word(0x20, 0x001F);
        gx.push_port_word(0x29, cull << 6);
        gx.push_port_word(0x40, 0);
        for w in [0xF000_F000u32, 0, 0xF000_1000, 0, 0x1000_0000, 0] {
            gx.push_port_word(0x23, w);
        }
        gx.push_port_word(0x50, 0);
        flush(&mut gx);
        gx.engine.fb[96 * 256 + 128] != 0
    }

    /// POLYGON_ATTR bits 6-7 select which faces are rendered: 0 neither,
    /// 1 back, 2 front, 3 both. One triangle has one facing, so exactly one of
    /// modes 1 and 2 must draw it — that is what makes culling meaningful,
    /// independent of which winding hardware calls "front".
    #[test]
    fn cull_mode_selects_faces() {
        assert!(!draws_with_cull(0), "mode 0 renders neither face");
        assert!(draws_with_cull(3), "mode 3 renders both faces");
        assert_ne!(
            draws_with_cull(1),
            draws_with_cull(2),
            "a single triangle faces exactly one way"
        );
    }

    /// Hardware alternates the winding of every other triangle in a strip. With
    /// culling on, failing to alternate makes every second triangle face the
    /// wrong way and vanish — so a 5-vertex strip must yield all 3 triangles.
    #[test]
    fn triangle_strip_keeps_every_triangle_under_culling() {
        let mut gx = GxDecoder::new();
        gx.push_port_word(0x20, 0x001F);
        // Render both faces, so this measures winding parity alone.
        gx.push_port_word(0x29, 0xC0);
        gx.push_port_word(0x40, 2); // triangle strip
        for w in [
            0xF000_F000u32, 0, 0xF000_1000, 0, 0x0000_F000, 0, 0x0000_1000, 0, 0x1000_F000, 0,
        ] {
            gx.push_port_word(0x23, w);
        }
        gx.push_port_word(0x50, 0);
        flush(&mut gx);
        assert_eq!(gx.engine.max_tris_per_frame, 3, "5 vertices = 3 triangles");
        assert_eq!(gx.engine.last_culled, 0, "cull mode 3 must keep all of them");
    }

    /// Milestone-1 rasterizer end-to-end: an identity-matrix NDC triangle
    /// flat-fills the framebuffer at its screen position (opaque bit set),
    /// and MTX_TRANS moves the follow-up triangle by the translated amount.
    #[test]
    fn geometry_flat_triangle_rasterizes_and_translates() {
        let mut gx = GxDecoder::new();
        gx.push_port_word(0x20, 0x001F); // COLOR red
        gx.push_port_word(0x29, 0xC0); // POLYGON_ATTR: render both faces
        gx.push_port_word(0x40, 0); // BEGIN triangles
        // (-1,-1,0) (1,-1,0) (0,1,0) as VTX_16 fx16 pairs, w=1.
        for w in [0xF000_F000u32, 0, 0xF000_1000, 0, 0x1000_0000, 0] {
            gx.push_port_word(0x23, w);
        }
        gx.push_port_word(0x50, 0); // SWAP_BUFFERS -> rasterize
        flush(&mut gx);
        assert_eq!(gx.engine.fb[96 * 256 + 128], 0x8000 | 0x001F, "center flat red");
        assert_eq!(gx.engine.fb[10 * 256 + 4], 0, "corner stays transparent");
        assert_eq!(gx.engine.max_tris_per_frame, 1);
        // Translate +1.0 in x and draw green: right half covered, left clear.
        gx.push_port_word(0x1C, 0x1000);
        gx.push_port_word(0x1C, 0);
        gx.push_port_word(0x1C, 0);
        gx.push_port_word(0x20, 0x03E0);
        gx.push_port_word(0x29, 0xC0); // POLYGON_ATTR: render both faces
        gx.push_port_word(0x40, 0);
        for w in [0xF000_F000u32, 0, 0xF000_1000, 0, 0x1000_0000, 0] {
            gx.push_port_word(0x23, w);
        }
        gx.push_port_word(0x50, 0);
        flush(&mut gx);
        assert_eq!(gx.engine.fb[96 * 256 + 224], 0x8000 | 0x03E0, "moved right");
        assert_eq!(gx.engine.fb[96 * 256 + 64], 0, "left half now clear");
    }

    /// Near-plane clipping (U34): a triangle straddling the near plane is
    /// split (1 vertex behind -> quad -> 2 tris; 2 behind -> 1 tri); all in
    /// front passes through as 1; all behind is dropped. The clip vertices
    /// land exactly on w = W_NEAR.
    #[test]
    fn near_plane_clip_splits_straddling_triangles() {
        let v = |w: f32| ClipV { pos: [0.0, 0.0, 0.0, w], col: [0.0; 3], uv: [0.0; 2] };
        // All in front -> unchanged single triangle.
        let (_, n) = clip_near_tri([v(1.0), v(1.0), v(1.0)]);
        assert_eq!(n, 1);
        // All behind -> nothing.
        let (_, n) = clip_near_tri([v(-1.0), v(-1.0), v(-1.0)]);
        assert_eq!(n, 0);
        // One behind -> 4-vertex polygon -> 2 triangles.
        let (out, n) = clip_near_tri([v(1.0), v(1.0), v(-1.0)]);
        assert_eq!(n, 2);
        for tri in out.iter().take(2) {
            for cv in tri {
                assert!(cv.pos[3] >= W_NEAR - 1e-6, "no clipped vertex behind near");
            }
        }
        // Two behind -> single clipped triangle.
        let (_, n) = clip_near_tri([v(1.0), v(-1.0), v(-1.0)]);
        assert_eq!(n, 1);
    }

    /// Vertex lighting: a camera-facing normal under a head-on white light
    /// lights the diffuse fully; a perpendicular normal gets nothing;
    /// ambient shows regardless of angle; and POLYGON_ATTR's light-enable
    /// bits only take effect when latched by BEGIN_VTXS.
    #[test]
    fn normal_lighting_diffuse_ambient_and_polygon_attr_gating() {
        let mut gx = GxDecoder::new();
        gx.push_port_word(0x32, 0x200 << 20); // light 0 vector (0,0,-1.0)
        gx.push_port_word(0x33, 0x7FFF); // light 0 white
        gx.push_port_word(0x30, 0x7FFF); // diffuse white, ambient black
        gx.push_port_word(0x31, 0); // no specular/emission
        // Lights disabled (POLYGON_ATTR bit0 clear, latched at BEGIN): black.
        gx.push_port_word(0x29, 0xC0); // cull field: render both faces
        gx.push_port_word(0x40, 0);
        gx.push_port_word(0x21, 0x1FF << 20); // normal (0,0,+0.998)
        assert_eq!(gx.engine.color, 0, "no enabled lights -> emission only");
        // Enabling light 0 without a new BEGIN_VTXS must not take effect yet.
        gx.push_port_word(0x29, 1);
        gx.push_port_word(0x21, 0x1FF << 20);
        assert_eq!(gx.engine.color, 0, "POLYGON_ATTR latches at BEGIN_VTXS");
        // Re-BEGIN so the light-enable bit set above latches; the cull field
        // must be carried along or the polygon renders no faces at all.
        gx.push_port_word(0x29, 0xC0 | 1);
        gx.push_port_word(0x40, 0);
        gx.push_port_word(0x21, 0x1FF << 20);
        assert_eq!(gx.engine.color, 0x7FFF, "facing normal fully lit");
        gx.push_port_word(0x21, 0x1FF); // normal (+0.998,0,0) perpendicular
        assert_eq!(gx.engine.color, 0, "perpendicular normal unlit");
        gx.push_port_word(0x30, 0x001F << 16); // ambient red, diffuse black
        gx.push_port_word(0x21, 0x1FF);
        assert_eq!(gx.engine.color, 0x001F, "ambient ignores light angle");
    }

    #[test]
    fn dif_amb_bit15_sets_vertex_color_and_clear_color_fills_on_swap() {
        let mut gx = GxDecoder::new();
        gx.push_port_word(0x30, 0x8000 | 0x03E0);
        assert_eq!(gx.engine.color, 0x03E0, "bit15 sets vertex color");
        gx.engine.clear_px = 0x8000 | 0x1234;
        gx.push_port_word(0x50, 0);
        assert!(gx.engine.swap_pending, "swap defers to the VRAM-lending owner");
        flush(&mut gx);
        assert!(!gx.engine.swap_pending);
        assert_eq!(gx.engine.fb[0], 0x9234, "swap fills with CLEAR_COLOR");
        assert_eq!(gx.engine.fb[191 * 256 + 255], 0x9234);
    }

    /// Strip emission: a 6-vertex quad strip is 2 quads = 4 triangles.
    #[test]
    fn quad_strip_emits_two_tris_per_pair() {
        let mut gx = GxDecoder::new();
        gx.push_port_word(0x29, 0xC0); // POLYGON_ATTR: render both faces
        gx.push_port_word(0x40, 3);
        for i in 0..6u32 {
            let x = (i / 2) * 0x0800; // 0, 0.5, 1.0
            let y = if i % 2 == 0 { 0u32 } else { 0x1000 };
            gx.push_port_word(0x23, x | (y << 16));
            gx.push_port_word(0x23, 0);
        }
        gx.push_port_word(0x50, 0);
        flush(&mut gx);
        assert_eq!(gx.engine.max_tris_per_frame, 4);
    }

    /// Build a VRAM with bank A as texture-image slot 0 and bank E as the
    /// texture-palette slots (both MST 3).
    fn tex_vram() -> VramManager {
        let mut vram = VramManager::new();
        vram.banks[0].control = 0x83; // A: enabled, MST 3, OFS 0
        vram.banks[4].control = 0x83; // E: enabled, MST 3
        vram
    }

    #[test]
    fn fetch_texel_decodes_pal16_color0_and_a3i5_alpha() {
        let mut vram = tex_vram();
        // Palette 0: entry1 = green, entry2 = blue, entry31 = red-ish.
        vram.banks[4].data[2..4].copy_from_slice(&0x03E0u16.to_le_bytes());
        vram.banks[4].data[4..6].copy_from_slice(&0x7C00u16.to_le_bytes());
        vram.banks[4].data[62..64].copy_from_slice(&0x001Fu16.to_le_bytes());
        // pal16 8x8 at texture offset 0: texel(0,0)=idx1, texel(1,0)=idx2.
        vram.banks[0].data[0] = 0x21;
        let pal16 = 3 << 26; // fmt=pal16, size 8x8, offset 0
        assert_eq!(fetch_texel(&vram, pal16, 0, 0, 0), Some((0x03E0, 31)));
        assert_eq!(fetch_texel(&vram, pal16, 0, 1, 0), Some((0x7C00, 31)));
        // texel(2,0)=idx0: opaque palette color 0 normally, transparent with
        // the color0 bit (29) set.
        assert_eq!(fetch_texel(&vram, pal16, 0, 2, 0), Some((0x0000, 31)));
        assert_eq!(fetch_texel(&vram, pal16 | 1 << 29, 0, 2, 0), None);
        // A3I5: byte = alpha3<<5 | idx5. alpha 0 -> transparent, partial
        // alpha scales 0-7 -> 0-31 (a3=1 -> 4).
        vram.banks[0].data[8] = 0x3F; // alpha 1, idx 31
        vram.banks[0].data[9] = 0x1F; // alpha 0, idx 31
        let a3i5 = (1 << 26) | 1; // fmt=A3I5, offset 1*8 bytes
        assert_eq!(fetch_texel(&vram, a3i5, 0, 0, 0), Some((0x001F, 4)));
        assert_eq!(fetch_texel(&vram, a3i5, 0, 1, 0), None);
        // Repeat wrap: x=8 with repeat_s wraps to x=0.
        assert_eq!(fetch_texel(&vram, pal16 | 1 << 16, 0, 8, 0), Some((0x03E0, 31)));
    }

    /// U31: the texture matrix is its own register — loading it must not
    /// clobber the lighting (vector) matrix — and texgen mode 1 transforms
    /// TEXCOORD through it (rows 2+3 = scroll translation /16).
    #[test]
    fn texture_matrix_texgen_and_lighting_isolation() {
        let mut gx = GxDecoder::new();
        // Light straight at the camera, white diffuse, light 0 enabled.
        gx.push_port_word(0x32, 0x200 << 20);
        gx.push_port_word(0x33, 0x7FFF);
        gx.push_port_word(0x30, 0x7FFF);
        gx.push_port_word(0x29, 0xC0 | 1);
        gx.push_port_word(0x40, 0);
        gx.push_port_word(0x21, 0x1FF << 20);
        assert_eq!(gx.engine.color, 0x7FFF);
        // Load garbage into the TEXTURE matrix (mode 3): lighting unchanged.
        gx.push_port_word(0x10, 3);
        for w in [0x2000u32, 0x2000, 0x2000, 0x2000, 0x2000, 0x2000, 0x2000, 0x2000, 0x2000] {
            gx.push_port_word(0x1A, w); // MTX_MULT_3x3 of 2.0s
        }
        gx.push_port_word(0x21, 0x1FF << 20);
        assert_eq!(gx.engine.color, 0x7FFF, "texture matrix must not touch lighting");
        // texgen 1: matrix row 3 translation of 16.0 shifts U by +1 texel.
        gx.push_port_word(0x15, 0); // MTX_IDENTITY (still mode 3)
        gx.push_port_word(0x1C, 0x10000); // MTX_TRANS x=16.0
        gx.push_port_word(0x1C, 0);
        gx.push_port_word(0x1C, 0);
        gx.push_port_word(0x2A, 1 << 30); // texgen mode 1
        gx.push_port_word(0x22, 0); // TEXCOORD (0,0)
        assert!((gx.engine.cur_uv[0] - 1.0).abs() < 1e-4, "u = trans/16 = 1 texel");
        assert!(gx.engine.cur_uv[1].abs() < 1e-4);
        // texgen 0 passes through untransformed.
        gx.push_port_word(0x2A, 0);
        gx.push_port_word(0x22, 32 << 4); // s = 32 texels
        assert!((gx.engine.cur_uv[0] - 32.0).abs() < 1e-4);
    }

    /// U31: a semi-transparent polygon (POLYGON_ATTR alpha 16) z-tested in
    /// front of an opaque one blends 16/31 new : 15/31 old.
    #[test]
    fn translucent_polygon_blends_over_opaque() {
        let mut gx = GxDecoder::new();
        // Opaque red triangle at z=0.5.
        gx.push_port_word(0x20, 0x001F);
        gx.push_port_word(0x29, 0xC0 | (31 << 16));
        gx.push_port_word(0x40, 0);
        for w in [0xF000_F000u32, 0x0800, 0xF000_1000, 0x0800, 0x1000_0000, 0x0800] {
            gx.push_port_word(0x23, w);
        }
        // Green alpha-16 triangle nearer (z=0).
        gx.push_port_word(0x20, 0x03E0);
        gx.push_port_word(0x29, 0xC0 | (16 << 16));
        gx.push_port_word(0x40, 0);
        for w in [0xF000_F000u32, 0, 0xF000_1000, 0, 0x1000_0000, 0] {
            gx.push_port_word(0x23, w);
        }
        gx.push_port_word(0x50, 0);
        flush(&mut gx);
        // r = 31*(15/31) = 15, g = 31*(16/31) = 16.
        assert_eq!(gx.engine.fb[96 * 256 + 128], 0x8000 | 15 | (16 << 5));
    }

    /// End-to-end: a textured full-screen triangle samples the texture and
    /// modulates it by the (white) vertex color.
    #[test]
    fn textured_triangle_rasterizes_palette_color() {
        let mut vram = tex_vram();
        vram.banks[4].data[2..4].copy_from_slice(&0x7C1Fu16.to_le_bytes()); // magenta
        vram.banks[0].data.iter_mut().take(32).for_each(|b| *b = 0x11); // all idx1
        let mut gx = GxDecoder::new();
        gx.push_port_word(0x2A, 3 << 26); // TEXIMAGE_PARAM pal16 8x8
        gx.push_port_word(0x2B, 0); // PLTT_BASE 0
        gx.push_port_word(0x20, 0x7FFF); // white vertex color
        gx.push_port_word(0x29, 0xC0); // POLYGON_ATTR: render both faces
        gx.push_port_word(0x40, 0);
        for (uv, w) in [
            (0u32, [0xF000_F000u32, 0]), // uv(0,0) at (-1,-1)
            (8 << 4, [0xF000_1000, 0]),  // uv(8,0) at (1,-1)
            ((8 << 4) << 16, [0x1000_0000, 0]), // uv(0,8) at (0,1)
        ] {
            gx.push_port_word(0x22, uv);
            gx.push_port_word(0x23, w[0]);
            gx.push_port_word(0x23, w[1]);
        }
        gx.push_port_word(0x50, 0);
        gx.engine.swap_buffers(&vram);
        assert_eq!(gx.engine.fb[96 * 256 + 128], 0x8000 | 0x7C1F, "modulated texel");
    }
}
