//! A minimal x86-64 encoder: the instructions the ARM9 recompiler emits, and
//! nothing else.
//!
//! # Why hand-rolled
//!
//! The alternative is a general assembler crate. This crate's only dependency is
//! `cxx`, and a recompiler needs perhaps thirty encodings — the ones below — so
//! a dependency would trade a large supply-chain and API surface for code that
//! fits on two screens and can be tested byte-for-byte against the
//! architecture manual.
//!
//! # Scope and safety
//!
//! Nothing here is `unsafe` and nothing here executes anything: the emitter
//! produces a `Vec<u8>`, which [`crate::jit::exec_mem`] is separately
//! responsible for making executable. That split is deliberate — encoding bugs
//! are ordinary logic bugs in a pure function, and can be found by comparing
//! bytes rather than by running them.
//!
//! # Operand size
//!
//! Guest ARM registers are 32-bit, so **the default operand size is 32-bit** and
//! the `_64` suffix marks the exceptions (pointers, the prologue and epilogue).
//! Writing a 32-bit register on x86-64 zero-extends into the full 64-bit
//! register, which is exactly the semantics a 32-bit guest wants and is why the
//! narrow forms are also the cheap ones.

/// The 16 general-purpose registers, numbered as the instruction encoding does.
///
/// `Rsp` is present because the encoding reserves its number, not because the
/// recompiler may allocate it; [`Mem`] documents where that number changes the
/// encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Reg {
    /// Low three bits, which go in ModRM or the opcode.
    fn low(self) -> u8 {
        self as u8 & 7
    }
    /// Fourth bit, which goes in the REX prefix.
    fn high(self) -> u8 {
        (self as u8 >> 3) & 1
    }
}

/// The registers by encoding number, so an opcode-extension `/digit` can be
/// turned into the ModRM reg field it shares a position with.
const REG_BY_NUMBER: [Reg; 8] =
    [Reg::Rax, Reg::Rcx, Reg::Rdx, Reg::Rbx, Reg::Rsp, Reg::Rbp, Reg::Rsi, Reg::Rdi];

/// A memory operand: `[base + disp]`. The recompiler never needs an index or a
/// scale — guest register slots are at fixed offsets from a pinned context
/// pointer, and guest memory goes through a call, not an addressing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mem {
    pub base: Reg,
    pub disp: i32,
}

impl Mem {
    pub fn new(base: Reg, disp: i32) -> Self {
        Self { base, disp }
    }
}

/// x86 condition codes, by their encoding. Named for the *flag test* rather than
/// for a signed/unsigned reading, so that mapping an ARM condition onto one is a
/// statement about flags and not about intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Cond {
    /// OF = 1
    O = 0x0,
    /// OF = 0
    No = 0x1,
    /// CF = 1 (unsigned below)
    B = 0x2,
    /// CF = 0 (unsigned above-or-equal)
    Ae = 0x3,
    /// ZF = 1
    E = 0x4,
    /// ZF = 0
    Ne = 0x5,
    /// CF = 1 or ZF = 1 (unsigned below-or-equal)
    Be = 0x6,
    /// CF = 0 and ZF = 0 (unsigned above)
    A = 0x7,
    /// SF = 1
    S = 0x8,
    /// SF = 0
    Ns = 0x9,
    /// SF != OF (signed less)
    L = 0xC,
    /// SF == OF (signed greater-or-equal)
    Ge = 0xD,
    /// ZF = 1 or SF != OF (signed less-or-equal)
    Le = 0xE,
    /// ZF = 0 and SF == OF (signed greater)
    G = 0xF,
}

/// The ALU operations that share the x86 "group 1" encoding shape, tagged with
/// the two things that vary: the `/digit` used with an immediate, and the base
/// opcode of the register form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    Add,
    Or,
    Adc,
    Sbb,
    And,
    Sub,
    Xor,
    Cmp,
}

impl AluOp {
    /// The `/digit` field for the immediate forms (`81 /digit`).
    fn digit(self) -> u8 {
        match self {
            Self::Add => 0,
            Self::Or => 1,
            Self::Adc => 2,
            Self::Sbb => 3,
            Self::And => 4,
            Self::Sub => 5,
            Self::Xor => 6,
            Self::Cmp => 7,
        }
    }
    /// Opcode of the `op r/m, r` form. The group is laid out at `digit * 8`.
    fn rm_r_opcode(self) -> u8 {
        self.digit() * 8 + 1
    }
}

/// The shift and rotate operations, by their `/digit` in groups 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftOp {
    Rol = 0,
    Ror = 1,
    /// Rotate right *through* the carry flag: CF becomes the new top bit and the
    /// old bottom bit becomes CF. This is exactly ARM's `RRX`.
    Rcr = 3,
    Shl = 4,
    Shr = 5,
    Sar = 7,
}

/// Accumulates encoded machine code.
///
/// Deliberately a plain `Vec<u8>` behind a method-per-instruction API: the
/// recompiler needs to emit, measure and patch, and all three are trivial on a
/// growable byte buffer.
#[derive(Debug, Default, Clone)]
pub struct Emitter {
    code: Vec<u8>,
}

impl Emitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes emitted so far. Also the offset the next byte will land at, which
    /// is what branch patching is expressed in.
    pub fn len(&self) -> usize {
        self.code.len()
    }

    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.code
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.code
    }

    // -- raw building blocks -------------------------------------------------

    fn byte(&mut self, b: u8) {
        self.code.push(b);
    }

    fn imm32(&mut self, v: i32) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    /// REX prefix, emitted only when it changes the encoding.
    ///
    /// `w` selects 64-bit operands; `r` and `b` are the fourth bits of the
    /// register and r/m fields. A REX byte is also required — with all fields
    /// zero — to reach `sil`/`dil`/`bpl`/`spl` in 8-bit forms, which is why
    /// [`Self::setcc`] passes `force`.
    fn rex(&mut self, w: bool, r: u8, b: u8, force: bool) {
        let byte = 0x40 | (u8::from(w) << 3) | (r << 2) | b;
        if w || r != 0 || b != 0 || force {
            self.byte(byte);
        }
    }

    /// ModRM for the register-to-register form (`mod` = 11).
    fn modrm_rr(&mut self, reg: Reg, rm: Reg) {
        self.byte(0xC0 | (reg.low() << 3) | rm.low());
    }

    /// ModRM (and SIB, and displacement) for a `[base + disp]` operand.
    ///
    /// Two encoding quirks are handled here rather than at each call site,
    /// because forgetting either produces a *different valid instruction* rather
    /// than an invalid one:
    ///
    /// * r/m == 4 (`rsp`, `r12`) means "a SIB byte follows", so a base of those
    ///   registers must emit one that names itself with no index.
    /// * r/m == 5 (`rbp`, `r13`) with mod == 00 means "RIP-relative", so those
    ///   bases must use an explicit zero displacement.
    fn modrm_mem(&mut self, reg: Reg, mem: Mem) {
        let needs_sib = mem.base.low() == 4;
        let force_disp8 = mem.base.low() == 5 && mem.disp == 0;

        let mode = if mem.disp == 0 && !force_disp8 {
            0b00
        } else if (-128..=127).contains(&mem.disp) {
            0b01
        } else {
            0b10
        };

        self.byte((mode << 6) | (reg.low() << 3) | mem.base.low());
        if needs_sib {
            // scale = 0, index = 100 (none), base = the register itself.
            self.byte(0x20 | mem.base.low());
        }
        match mode {
            0b01 => self.byte(mem.disp as u8),
            0b10 => self.imm32(mem.disp),
            _ => {}
        }
    }

    // -- moves ---------------------------------------------------------------

    /// `mov dst32, src32`
    pub fn mov_rr(&mut self, dst: Reg, src: Reg) {
        self.rex(false, src.high(), dst.high(), false);
        self.byte(0x89); // mov r/m32, r32
        self.modrm_rr(src, dst);
    }

    /// `mov dst64, src64`
    pub fn mov_rr_64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, src.high(), dst.high(), false);
        self.byte(0x89);
        self.modrm_rr(src, dst);
    }

    /// `mov dst32, imm32`
    pub fn mov_ri(&mut self, dst: Reg, imm: u32) {
        self.rex(false, 0, dst.high(), false);
        self.byte(0xB8 + dst.low());
        self.imm32(imm as i32);
    }

    /// `mov dst64, imm64`. Ten bytes; used for thunk addresses.
    pub fn mov_ri_64(&mut self, dst: Reg, imm: u64) {
        self.rex(true, 0, dst.high(), false);
        self.byte(0xB8 + dst.low());
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// `mov dst32, [src + disp]`
    pub fn mov_rm(&mut self, dst: Reg, src: Mem) {
        self.rex(false, dst.high(), src.base.high(), false);
        self.byte(0x8B); // mov r32, r/m32
        self.modrm_mem(dst, src);
    }

    /// `mov [dst + disp], src32`
    pub fn mov_mr(&mut self, dst: Mem, src: Reg) {
        self.rex(false, src.high(), dst.base.high(), false);
        self.byte(0x89); // mov r/m32, r32
        self.modrm_mem(src, dst);
    }

    /// `lea dst64, [src + disp]` — the address, not the contents.
    ///
    /// Used to hand a thunk a pointer to a buffer inside the context.
    pub fn lea_64(&mut self, dst: Reg, src: Mem) {
        self.rex(true, dst.high(), src.base.high(), false);
        self.byte(0x8D);
        self.modrm_mem(dst, src);
    }

    /// `mov dst64, [src + disp]`
    pub fn mov_rm_64(&mut self, dst: Reg, src: Mem) {
        self.rex(true, dst.high(), src.base.high(), false);
        self.byte(0x8B);
        self.modrm_mem(dst, src);
    }

    /// `mov [dst + disp], imm32`
    pub fn mov_mi(&mut self, dst: Mem, imm: u32) {
        self.rex(false, 0, dst.base.high(), false);
        self.byte(0xC7); // mov r/m32, imm32
        self.modrm_mem(Reg::Rax, dst); // reg field is the /0 digit
        self.imm32(imm as i32);
    }

    /// `movzx dst32, byte [src + disp]`
    pub fn movzx_rm8(&mut self, dst: Reg, src: Mem) {
        self.rex(false, dst.high(), src.base.high(), false);
        self.byte(0x0F);
        self.byte(0xB6);
        self.modrm_mem(dst, src);
    }

    /// `movsx dst32, src8` — sign-extend the low byte (`LDRSB`).
    ///
    /// No forced REX: only `al`-class sources are emitted, and forcing one for
    /// `spl`/`bpl`/`sil`/`dil` reachability would silently change which
    /// register the legacy encoding names.
    pub fn movsx_rr8(&mut self, dst: Reg, src: Reg) {
        self.rex(false, dst.high(), src.high(), false);
        self.byte(0x0F);
        self.byte(0xBE);
        self.modrm_rr(dst, src);
    }

    /// `movsx dst32, src16` — sign-extend the low halfword (`LDRSH`).
    pub fn movsx_rr16(&mut self, dst: Reg, src: Reg) {
        self.rex(false, dst.high(), src.high(), false);
        self.byte(0x0F);
        self.byte(0xBF);
        self.modrm_rr(dst, src);
    }

    /// `movsxd dst64, src32` — sign-extend a doubleword into 64 bits: the
    /// signed long-multiply operands, and the trick that lets one 64-bit
    /// `test` read a 32-bit result's N and Z (bit 63 mirrors bit 31).
    pub fn movsxd_rr(&mut self, dst: Reg, src: Reg) {
        self.rex(true, dst.high(), src.high(), false);
        self.byte(0x63);
        self.modrm_rr(dst, src);
    }

    /// `bsr dst32, src32` — index of the highest set bit; ZF says the source
    /// was zero (in which case `dst` is architecturally undefined and must
    /// not be consumed). What `CLZ` is built from: `31 - bsr`, with the zero
    /// case branched around.
    pub fn bsr_rr(&mut self, dst: Reg, src: Reg) {
        self.rex(false, dst.high(), src.high(), false);
        self.byte(0x0F);
        self.byte(0xBD);
        self.modrm_rr(dst, src);
    }

    // -- ALU -----------------------------------------------------------------

    /// `<op> dst32, src32`
    pub fn alu_rr(&mut self, op: AluOp, dst: Reg, src: Reg) {
        self.rex(false, src.high(), dst.high(), false);
        self.byte(op.rm_r_opcode());
        self.modrm_rr(src, dst);
    }

    /// `<op> dst32, imm32`
    pub fn alu_ri(&mut self, op: AluOp, dst: Reg, imm: u32) {
        self.rex(false, 0, dst.high(), false);
        self.byte(0x81); // group 1, r/m32, imm32
        self.byte(0xC0 | (op.digit() << 3) | dst.low());
        self.imm32(imm as i32);
    }

    /// `<op> dword [dst + disp], imm32`
    pub fn alu_mi(&mut self, op: AluOp, dst: Mem, imm: u32) {
        self.rex(false, 0, dst.base.high(), false);
        self.byte(0x81); // group 1, r/m32, imm32
        // The ModRM reg field carries the `/digit`; any `Reg` with those low
        // three bits would do, and naming it by number is what the manual means.
        self.modrm_mem(REG_BY_NUMBER[op.digit() as usize], dst);
        self.imm32(imm as i32);
    }

    /// `<op> dst32, [src + disp]`
    pub fn alu_rm(&mut self, op: AluOp, dst: Reg, src: Mem) {
        self.rex(false, dst.high(), src.base.high(), false);
        self.byte(op.rm_r_opcode() + 2); // the `op r, r/m` direction
        self.modrm_mem(dst, src);
    }

    /// `add dst64, imm32` (sign-extended). Stack adjustment in the prologue.
    pub fn alu_ri_64(&mut self, op: AluOp, dst: Reg, imm: i32) {
        self.rex(true, 0, dst.high(), false);
        self.byte(0x81);
        self.byte(0xC0 | (op.digit() << 3) | dst.low());
        self.imm32(imm);
    }

    /// `<op> dst64, src64`. The dispatch exit's pointer arithmetic.
    pub fn alu_rr_64(&mut self, op: AluOp, dst: Reg, src: Reg) {
        self.rex(true, src.high(), dst.high(), false);
        self.byte(op.rm_r_opcode());
        self.modrm_rr(src, dst);
    }

    /// `imul dst64, src64` — the multiply half of the dispatch hash. Truncating
    /// 64x64, matching `u64::wrapping_mul`.
    pub fn imul_rr_64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, dst.high(), src.high(), false);
        self.byte(0x0F);
        self.byte(0xAF);
        self.modrm_rr(dst, src);
    }

    /// `<op> dst64, imm-amount` — the 64-bit shift the dispatch hash needs to
    /// take the product's high half.
    pub fn shift_ri_64(&mut self, op: ShiftOp, dst: Reg, amount: u8) {
        self.rex(true, 0, dst.high(), false);
        self.byte(0xC1);
        self.byte(0xC0 | ((op as u8) << 3) | dst.low());
        self.byte(amount);
    }

    /// `not dst32`
    pub fn not_r(&mut self, dst: Reg) {
        self.rex(false, 0, dst.high(), false);
        self.byte(0xF7);
        self.byte(0xC0 | (2 << 3) | dst.low());
    }

    /// `neg dst32`
    pub fn neg_r(&mut self, dst: Reg) {
        self.rex(false, 0, dst.high(), false);
        self.byte(0xF7);
        self.byte(0xC0 | (3 << 3) | dst.low());
    }

    /// `test dst32, src32`
    pub fn test_rr(&mut self, dst: Reg, src: Reg) {
        self.rex(false, src.high(), dst.high(), false);
        self.byte(0x85);
        self.modrm_rr(src, dst);
    }

    /// `imul dst32, src32`
    pub fn imul_rr(&mut self, dst: Reg, src: Reg) {
        self.rex(false, dst.high(), src.high(), false);
        self.byte(0x0F);
        self.byte(0xAF);
        self.modrm_rr(dst, src);
    }

    // -- shifts --------------------------------------------------------------

    /// `<op> dst32, imm8`. An amount of zero is *not* encoded as a shift by
    /// zero — it is skipped, because on x86 a shift by zero leaves the flags
    /// alone while the ARM semantics for "shift by zero" are a separate case the
    /// translator handles explicitly.
    pub fn shift_ri(&mut self, op: ShiftOp, dst: Reg, amount: u8) {
        self.rex(false, 0, dst.high(), false);
        self.byte(0xC1);
        self.byte(0xC0 | ((op as u8) << 3) | dst.low());
        self.byte(amount);
    }

    /// `<op> dst32, cl`
    pub fn shift_rcl(&mut self, op: ShiftOp, dst: Reg) {
        self.rex(false, 0, dst.high(), false);
        self.byte(0xD3);
        self.byte(0xC0 | ((op as u8) << 3) | dst.low());
    }

    // -- flags ---------------------------------------------------------------

    /// `set<cc> dst8`, zeroing the rest of `dst` is *not* implied — the caller
    /// pairs this with `movzx` when it needs a clean 0/1 in 32 bits.
    ///
    /// Forces a REX prefix so `sil`/`dil`/`bpl`/`spl` are reachable; without it
    /// those encodings mean `ah`/`ch`/`dh`/`bh` instead, which is a silently
    /// wrong register rather than an error.
    pub fn setcc(&mut self, cond: Cond, dst: Reg) {
        self.rex(false, 0, dst.high(), true);
        self.byte(0x0F);
        self.byte(0x90 + cond as u8);
        self.byte(0xC0 | dst.low());
    }

    /// `cmc` — complement the carry flag. ARM's carry for a subtraction is the
    /// inverse of x86's, so this is how a `SUB` publishes the right C.
    pub fn cmc(&mut self) {
        self.byte(0xF5);
    }

    /// `lahf` — load SF, ZF, AF, PF and CF into `ah`.
    ///
    /// One byte for five flags, which is why flag extraction is built around it
    /// rather than around five `setcc`s. It does **not** include OF; pair it
    /// with `seto` for that. It writes `ah`, so the value being measured must
    /// already be somewhere other than `rax`.
    pub fn lahf(&mut self) {
        self.byte(0x9F);
    }

    /// `bt src32, bit` — copy bit `bit` of `src` into CF.
    ///
    /// The cheapest way to get a guest carry flag back into the host's, which is
    /// what `ADC`/`SBC`/`RSC` need before their x86 counterpart runs.
    pub fn bt_ri(&mut self, src: Reg, bit: u8) {
        self.rex(false, 0, src.high(), false);
        self.byte(0x0F);
        self.byte(0xBA);
        self.byte(0xC0 | (4 << 3) | src.low());
        self.byte(bit);
    }

    /// `inc dword [base + disp]`
    pub fn inc_m(&mut self, dst: Mem) {
        self.rex(false, 0, dst.base.high(), false);
        self.byte(0xFF);
        self.modrm_mem(Reg::Rax, dst); // reg field is the /0 digit
    }

    // -- control flow --------------------------------------------------------

    pub fn push_64(&mut self, reg: Reg) {
        self.rex(false, 0, reg.high(), false);
        self.byte(0x50 + reg.low());
    }

    pub fn pop_64(&mut self, reg: Reg) {
        self.rex(false, 0, reg.high(), false);
        self.byte(0x58 + reg.low());
    }

    /// `call reg` (indirect, 64-bit operand implied).
    /// `jmp qword [mem]` — an indirect tail jump through a memory slot.
    ///
    /// This is how one compiled block reaches its successor. The alternative —
    /// patching a direct `jmp rel32` into published code — needs an RX -> RW
    /// flip per link, which is the operation Arbitrary Code Guard can refuse
    /// outright and the one whose error path already produced a use-after-free
    /// here once. Jumping through a slot makes linking a plain data write, so
    /// no page ever leaves `PAGE_EXECUTE_READ` and no new `unsafe` is needed.
    ///
    /// `FF /4`, and 64-bit operand size is implicit for a jump in long mode, so
    /// no REX.W: the only REX needed is for a high base register.
    pub fn jmp_m(&mut self, src: Mem) {
        self.rex(false, 0, src.base.high(), false);
        self.byte(0xFF);
        self.modrm_mem(Reg::Rsp, src); // reg field is the /4 digit
    }

    pub fn call_r(&mut self, reg: Reg) {
        self.rex(false, 0, reg.high(), false);
        self.byte(0xFF);
        self.byte(0xC0 | (2 << 3) | reg.low());
    }

    /// `jmp reg` (indirect, 64-bit operand implied). The dispatch probe's
    /// landing jump, taken only after the loaded body pointer proved non-null.
    pub fn jmp_r(&mut self, reg: Reg) {
        self.rex(false, 0, reg.high(), false);
        self.byte(0xFF);
        self.byte(0xC0 | (4 << 3) | reg.low());
    }

    /// `test dst64, src64` — the null check on a loaded code pointer.
    pub fn test_rr_64(&mut self, dst: Reg, src: Reg) {
        self.rex(true, src.high(), dst.high(), false);
        self.byte(0x85);
        self.modrm_rr(src, dst);
    }

    pub fn ret(&mut self) {
        self.byte(0xC3);
    }

    /// Emit `j<cc> rel32` with a placeholder target; returns the offset of the
    /// displacement field for [`Self::patch_rel32`].
    pub fn jcc_placeholder(&mut self, cond: Cond) -> usize {
        self.byte(0x0F);
        self.byte(0x80 + cond as u8);
        let at = self.len();
        self.imm32(0);
        at
    }

    /// Emit `jmp rel32` with a placeholder target; see [`Self::jcc_placeholder`].
    pub fn jmp_placeholder(&mut self) -> usize {
        self.byte(0xE9);
        let at = self.len();
        self.imm32(0);
        at
    }

    /// Point a placeholder emitted at `site` to the current end of the buffer.
    ///
    /// x86 branch displacements are relative to the *end* of the branch
    /// instruction, which for these forms is four bytes past `site`.
    pub fn patch_rel32(&mut self, site: usize) {
        let target = self.len();
        let rel = (target as i64) - (site as i64 + 4);
        let rel = rel as i32;
        self.code[site..site + 4].copy_from_slice(&rel.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode one instruction and compare against the manual's bytes.
    fn enc(f: impl FnOnce(&mut Emitter)) -> Vec<u8> {
        let mut e = Emitter::new();
        f(&mut e);
        e.into_vec()
    }

    /// `movsxd r64, r/m32` is REX.W 63 /r — checked against hand assembly for
    /// a low/low and a high/high pair so both REX extension bits are covered.
    #[test]
    fn movsxd_sign_extends_a_doubleword() {
        assert_eq!(enc(|e| e.movsxd_rr(Reg::Rax, Reg::Rcx)), vec![0x48, 0x63, 0xC1]);
        assert_eq!(enc(|e| e.movsxd_rr(Reg::R8, Reg::R9)), vec![0x4D, 0x63, 0xC1]);
    }

    /// `bsr r32, r/m32` is 0F BD /r.
    #[test]
    fn bsr_finds_the_highest_set_bit() {
        assert_eq!(enc(|e| e.bsr_rr(Reg::Rcx, Reg::Rdx)), vec![0x0F, 0xBD, 0xCA]);
        assert_eq!(enc(|e| e.bsr_rr(Reg::R9, Reg::Rdx)), vec![0x44, 0x0F, 0xBD, 0xCA]);
    }

    #[test]
    fn moves_between_registers() {
        assert_eq!(enc(|e| e.mov_rr(Reg::Rax, Reg::Rcx)), [0x89, 0xC8], "mov eax, ecx");
        // REX.B reaches r8 as the destination, REX.R as the source.
        assert_eq!(enc(|e| e.mov_rr(Reg::R8, Reg::Rcx)), [0x41, 0x89, 0xC8], "mov r8d, ecx");
        assert_eq!(enc(|e| e.mov_rr(Reg::Rax, Reg::R9)), [0x44, 0x89, 0xC8], "mov eax, r9d");
        assert_eq!(enc(|e| e.mov_rr_64(Reg::Rbx, Reg::Rcx)), [0x48, 0x89, 0xCB], "mov rbx, rcx");
    }

    /// The sign-extensions the halfword transfers added.
    #[test]
    fn sign_extensions_encode_as_documented() {
        assert_eq!(enc(|e| e.movsx_rr8(Reg::Rax, Reg::Rax)), [0x0F, 0xBE, 0xC0], "movsx eax, al");
        assert_eq!(enc(|e| e.movsx_rr16(Reg::Rax, Reg::Rax)), [0x0F, 0xBF, 0xC0], "movsx eax, ax");
    }

    /// The three encodings the dispatch hash added, against the manual's bytes.
    #[test]
    fn dispatch_hash_arithmetic_encodes_as_documented() {
        assert_eq!(
            enc(|e| e.imul_rr_64(Reg::Rcx, Reg::R8)),
            [0x49, 0x0F, 0xAF, 0xC8],
            "imul rcx, r8"
        );
        assert_eq!(
            enc(|e| e.shift_ri_64(ShiftOp::Shr, Reg::Rcx, 32)),
            [0x48, 0xC1, 0xE9, 0x20],
            "shr rcx, 32"
        );
        assert_eq!(
            enc(|e| e.alu_rr_64(AluOp::Add, Reg::R8, Reg::Rcx)),
            [0x49, 0x01, 0xC8],
            "add r8, rcx"
        );
    }

    #[test]
    fn immediate_moves() {
        assert_eq!(
            enc(|e| e.mov_ri(Reg::Rax, 0x1234_5678)),
            [0xB8, 0x78, 0x56, 0x34, 0x12],
            "mov eax, 0x12345678"
        );
        assert_eq!(
            enc(|e| e.mov_ri_64(Reg::R11, 0xDEAD_BEEF_CAFE_1234)),
            [0x49, 0xBB, 0x34, 0x12, 0xFE, 0xCA, 0xEF, 0xBE, 0xAD, 0xDE],
            "movabs r11, imm64"
        );
    }

    /// The three ModRM traps, each of which encodes a *different valid
    /// instruction* if it is missed rather than failing loudly.
    #[test]
    fn memory_operand_encoding_quirks() {
        // Ordinary base, zero displacement: mod = 00, no displacement bytes.
        assert_eq!(
            enc(|e| e.mov_rm(Reg::Rax, Mem::new(Reg::Rbx, 0))),
            [0x8B, 0x03],
            "mov eax, [rbx]"
        );
        // rbp with a zero displacement must still emit disp8, or mod = 00
        // would mean RIP-relative.
        assert_eq!(
            enc(|e| e.mov_rm(Reg::Rax, Mem::new(Reg::Rbp, 0))),
            [0x8B, 0x45, 0x00],
            "mov eax, [rbp+0]"
        );
        // rsp as a base needs a SIB byte naming itself with no index.
        assert_eq!(
            enc(|e| e.mov_rm(Reg::Rax, Mem::new(Reg::Rsp, 0))),
            [0x8B, 0x04, 0x24],
            "mov eax, [rsp]"
        );
        // r12 has the same low three bits as rsp, so it needs SIB too...
        assert_eq!(
            enc(|e| e.mov_rm(Reg::Rax, Mem::new(Reg::R12, 0))),
            [0x41, 0x8B, 0x04, 0x24],
            "mov eax, [r12]"
        );
        // ...and r13 shares rbp's, so it needs the forced displacement.
        assert_eq!(
            enc(|e| e.mov_rm(Reg::Rax, Mem::new(Reg::R13, 0))),
            [0x41, 0x8B, 0x45, 0x00],
            "mov eax, [r13+0]"
        );
    }

    #[test]
    fn displacement_width_is_chosen_by_magnitude() {
        assert_eq!(
            enc(|e| e.mov_rm(Reg::Rax, Mem::new(Reg::Rbx, 0x40))),
            [0x8B, 0x43, 0x40],
            "disp8"
        );
        assert_eq!(
            enc(|e| e.mov_rm(Reg::Rax, Mem::new(Reg::Rbx, 0x100))),
            [0x8B, 0x83, 0x00, 0x01, 0x00, 0x00],
            "disp32"
        );
        assert_eq!(
            enc(|e| e.mov_rm(Reg::Rax, Mem::new(Reg::Rbx, -1))),
            [0x8B, 0x43, 0xFF],
            "negative disp8"
        );
    }

    #[test]
    fn alu_forms() {
        assert_eq!(enc(|e| e.alu_rr(AluOp::Add, Reg::Rax, Reg::Rcx)), [0x01, 0xC8], "add eax, ecx");
        assert_eq!(enc(|e| e.alu_rr(AluOp::Sub, Reg::Rax, Reg::Rcx)), [0x29, 0xC8], "sub eax, ecx");
        assert_eq!(enc(|e| e.alu_rr(AluOp::And, Reg::Rax, Reg::Rcx)), [0x21, 0xC8], "and eax, ecx");
        assert_eq!(enc(|e| e.alu_rr(AluOp::Or, Reg::Rax, Reg::Rcx)), [0x09, 0xC8], "or eax, ecx");
        assert_eq!(enc(|e| e.alu_rr(AluOp::Xor, Reg::Rax, Reg::Rcx)), [0x31, 0xC8], "xor eax, ecx");
        assert_eq!(enc(|e| e.alu_rr(AluOp::Cmp, Reg::Rax, Reg::Rcx)), [0x39, 0xC8], "cmp eax, ecx");
        assert_eq!(enc(|e| e.alu_rr(AluOp::Adc, Reg::Rax, Reg::Rcx)), [0x11, 0xC8], "adc eax, ecx");
        assert_eq!(enc(|e| e.alu_rr(AluOp::Sbb, Reg::Rax, Reg::Rcx)), [0x19, 0xC8], "sbb eax, ecx");

        assert_eq!(
            enc(|e| e.alu_ri(AluOp::Add, Reg::Rdx, 0x10)),
            [0x81, 0xC2, 0x10, 0x00, 0x00, 0x00],
            "add edx, 0x10"
        );
        assert_eq!(
            enc(|e| e.alu_rm(AluOp::Add, Reg::Rax, Mem::new(Reg::Rbx, 8))),
            [0x03, 0x43, 0x08],
            "add eax, [rbx+8]"
        );
    }

    #[test]
    fn shifts_and_unary() {
        assert_eq!(enc(|e| e.shift_ri(ShiftOp::Shl, Reg::Rax, 3)), [0xC1, 0xE0, 0x03], "shl eax, 3");
        assert_eq!(enc(|e| e.shift_ri(ShiftOp::Shr, Reg::Rax, 1)), [0xC1, 0xE8, 0x01], "shr eax, 1");
        assert_eq!(enc(|e| e.shift_ri(ShiftOp::Sar, Reg::Rax, 5)), [0xC1, 0xF8, 0x05], "sar eax, 5");
        assert_eq!(enc(|e| e.shift_ri(ShiftOp::Ror, Reg::Rax, 7)), [0xC1, 0xC8, 0x07], "ror eax, 7");
        assert_eq!(enc(|e| e.shift_rcl(ShiftOp::Shl, Reg::Rdx)), [0xD3, 0xE2], "shl edx, cl");
        assert_eq!(enc(|e| e.not_r(Reg::Rax)), [0xF7, 0xD0], "not eax");
        assert_eq!(enc(|e| e.neg_r(Reg::Rcx)), [0xF7, 0xD9], "neg ecx");
    }

    /// `setcc` on `sil`/`dil` needs a REX prefix even though no register is
    /// extended; without it the encoding names `dh` instead.
    #[test]
    fn setcc_forces_rex_for_the_low_byte_registers() {
        assert_eq!(enc(|e| e.setcc(Cond::E, Reg::Rax)), [0x40, 0x0F, 0x94, 0xC0], "sete al");
        assert_eq!(enc(|e| e.setcc(Cond::B, Reg::Rsi)), [0x40, 0x0F, 0x92, 0xC6], "setb sil");
        assert_eq!(enc(|e| e.setcc(Cond::O, Reg::R10)), [0x41, 0x0F, 0x90, 0xC2], "seto r10b");
    }

    #[test]
    fn lea_computes_an_address() {
        assert_eq!(enc(|e| e.lea_64(Reg::R8, Mem::new(Reg::R12, 56))), [0x4D, 0x8D, 0x44, 0x24, 0x38]);
        assert_eq!(enc(|e| e.lea_64(Reg::Rcx, Mem::new(Reg::Rbx, 0))), [0x48, 0x8D, 0x0B]);
    }

    #[test]
    fn flag_transfer_forms() {
        assert_eq!(enc(|e| e.lahf()), [0x9F], "lahf");
        assert_eq!(enc(|e| e.bt_ri(Reg::Rsi, 29)), [0x0F, 0xBA, 0xE6, 0x1D], "bt esi, 29");
        assert_eq!(enc(|e| e.bt_ri(Reg::R13, 29)), [0x41, 0x0F, 0xBA, 0xE5, 0x1D], "bt r13d, 29");
        assert_eq!(enc(|e| e.inc_m(Mem::new(Reg::R12, 24))), [0x41, 0xFF, 0x44, 0x24, 0x18]);
    }

    /// `bt` publishes the selected bit in CF, which is what `adc` then consumes.
    /// Checked on the host because a wrong `/digit` here would silently select
    /// `bts`/`btr`/`btc` — all of which also *modify* the source.
    #[test]
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn bt_moves_a_bit_into_the_carry_flag() {
        use crate::jit::exec_mem::CodeBuffer;

        for (bits, expected) in [(0u32, 10u32), (1 << 29, 11)] {
            let mut e = Emitter::new();
            // RSI is **callee-saved** under Win64, and this buffer is called as
            // an ordinary `extern "C"` function from Rust. Clobbering it and
            // returning corrupted the caller's register: harmless in a debug
            // build, where nothing was live there, and a hard
            // `STATUS_ACCESS_VIOLATION` in release, where `lto = true` and
            // `codegen-units = 1` keep a live pointer across the call. The
            // whole release test binary died on it, which is why gate 1 is
            // specified as a debug run and never caught it.
            //
            // Preserved rather than swapped for a volatile register so the
            // encoding under test — `bt esi, 29` — is the one that ships.
            e.push_64(Reg::Rsi);
            e.mov_ri(Reg::Rsi, bits);
            e.mov_ri(Reg::Rax, 10);
            e.bt_ri(Reg::Rsi, 29);
            e.alu_ri(AluOp::Adc, Reg::Rax, 0); // eax += 0 + CF
            e.pop_64(Reg::Rsi);
            e.ret();

            let mut buf = CodeBuffer::with_capacity(64).unwrap();
            buf.push(e.as_slice()).unwrap();
            let exec = buf.finalize().unwrap();
            assert_eq!(exec.entry(0).unwrap()(std::ptr::null_mut()), expected, "bits {bits:#x}");
            // `bt` must not have modified esi; `bts` and friends would have.
        }
    }

    #[test]
    fn stack_and_calls() {
        assert_eq!(enc(|e| e.push_64(Reg::Rbx)), [0x53], "push rbx");
        assert_eq!(enc(|e| e.push_64(Reg::R12)), [0x41, 0x54], "push r12");
        assert_eq!(enc(|e| e.pop_64(Reg::R15)), [0x41, 0x5F], "pop r15");
        assert_eq!(enc(|e| e.call_r(Reg::Rax)), [0xFF, 0xD0], "call rax");
        assert_eq!(enc(|e| e.ret()), [0xC3], "ret");
        assert_eq!(enc(|e| e.cmc()), [0xF5], "cmc");
    }

    /// A displacement is relative to the end of the branch, not to its start.
    /// An off-by-four here lands mid-instruction, which executes as something
    /// else entirely rather than faulting.
    #[test]
    fn branch_displacements_are_end_relative() {
        let mut e = Emitter::new();
        let site = e.jmp_placeholder();
        assert_eq!(e.len(), 5, "jmp rel32 is five bytes");
        e.ret(); // one byte to jump over
        e.patch_rel32(site);
        assert_eq!(e.as_slice(), &[0xE9, 0x01, 0x00, 0x00, 0x00, 0xC3], "jmp +1 over the ret");

        let mut e = Emitter::new();
        let site = e.jcc_placeholder(Cond::Ne);
        e.patch_rel32(site);
        assert_eq!(e.as_slice(), &[0x0F, 0x85, 0x00, 0x00, 0x00, 0x00], "jne to the next byte");
    }

    /// End-to-end: encode a function, run it on the host, check the result.
    /// The byte assertions above prove the encoding matches the manual; this
    /// proves the manual was read correctly.
    #[test]
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn the_host_agrees_with_the_encoder() {
        use crate::jit::exec_mem::CodeBuffer;

        // `mov eax, 100 ; sub eax, 58 ; ret` -> 42, with a borrow-free subtract.
        let mut e = Emitter::new();
        e.mov_ri(Reg::Rax, 100);
        e.alu_ri(AluOp::Sub, Reg::Rax, 58);
        e.ret();

        let mut buf = CodeBuffer::with_capacity(64).expect("reserve");
        buf.push(e.as_slice()).expect("emit");
        let exec = buf.finalize().expect("RW -> RX");
        assert_eq!(exec.entry(0).unwrap()(std::ptr::null_mut()), 42);
    }

    /// ...and that a conditional branch patched by [`Emitter::patch_rel32`]
    /// really lands where it was pointed.
    #[test]
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn a_patched_branch_lands_on_its_target() {
        use crate::jit::exec_mem::CodeBuffer;

        // eax = 1; if (eax == 1) goto done; eax = 999; done: ret
        let mut e = Emitter::new();
        e.mov_ri(Reg::Rax, 1);
        e.alu_ri(AluOp::Cmp, Reg::Rax, 1);
        let taken = e.jcc_placeholder(Cond::E);
        e.mov_ri(Reg::Rax, 999);
        e.patch_rel32(taken);
        e.ret();

        let mut buf = CodeBuffer::with_capacity(64).unwrap();
        buf.push(e.as_slice()).unwrap();
        let exec = buf.finalize().unwrap();
        assert_eq!(exec.entry(0).unwrap()(std::ptr::null_mut()), 1, "the branch skipped the 999");
    }
}
