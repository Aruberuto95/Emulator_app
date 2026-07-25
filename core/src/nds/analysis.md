# NDS Emulation Module Design Analysis: Dual-Core, MMU, IPC, and HLE Booting

## Overview
This document presents the architectural design and structural recommendations for the Nintendo DS (NDS) emulation module. The goal is to support direct High-Level Emulation (HLE) booting of NDS ROMs (such as *Pokémon SoulSilver*), memory-mapped input/output (MMU) with dynamic VRAM/WRAM banks, IPC registers for CPU communication, and a synchronized dual-core execution model in Rust.

---

## 1. Dual-Core CPU Execution Structure

The NDS features two ARM processor cores:
1. **ARM946E-S** (ARM9): 67.03 MHz (ARMv5TE instruction set, CP15 coprocessor for cache/TCM/MPU control).
2. **ARM7TDMI** (ARM7): 33.51 MHz (ARMv4T instruction set, identical to the GBA core).

### Rust Coordination Structure
To tick both cores without violating Rust's ownership rules, we separate the execution states from the shared memory bus. We propose a parent coordinator struct `Nds` containing the cores and the memory controller:

```rust
pub struct Nds {
    pub arm9: Arm9Cpu,
    pub arm7: Arm7Cpu,
    pub mmu: NdsMmu,
    pub cycle_accumulator_9: u64,
    pub cycle_accumulator_7: u64,
}
```

### Tick Interleaving and Cycle Budgeting
The ARM9 runs exactly twice as fast as the ARM7 (ratio 2:1). In the main emulation loop:
- A frame consists of 263 scanlines.
- Total cycles per frame: **560,190 ARM9 cycles** and **280,095 ARM7 cycles**.
- To balance synchronization latency and performance, execution is interleaved in small batches (e.g., scanline-sized slices of ~2132 ARM9 cycles / ~1066 ARM7 cycles) rather than instruction-by-instruction.
- If either CPU triggers an IPC action (such as writing to `IPCSYNC` with Send IRQ set or writing to `IPCFIFO`), a sync trigger is flagged, catching up the other CPU to maintain causal order.

```rust
impl Nds {
    pub fn tick(&mut self, base_cycles: u32, speed: f32) {
        let arm9_budget = (base_cycles as f32 * speed) as u32;
        let mut arm9_run = 0;
        let mut arm7_run = 0;

        while arm9_run < arm9_budget {
            let slice_9 = std::cmp::min(2132, arm9_budget - arm9_run);
            let slice_7 = slice_9 / 2;

            // Run ARM9
            let mut bus_9 = Arm9Bus {
                mmu: &mut self.mmu,
                dtcm: &mut self.arm9.dtcm,
                itcm: &mut self.arm9.itcm,
                cp15: &self.arm9.cp15,
            };
            arm9_run += self.arm9.execute_cycles(slice_9, &mut bus_9);

            // Run ARM7
            let mut bus_7 = Arm7Bus {
                mmu: &mut self.mmu,
            };
            arm7_run += self.arm7.execute_cycles(slice_7, &mut bus_7);

            // Process interrupts and check if catch-up is needed
            self.mmu.check_ipc_interrupts(&mut self.arm9, &mut self.arm7);
        }
    }
}
```

### ARM946E-S (ARMv5TE) vs. ARM7TDMI (ARMv4T)
The ARM9 CPU core must implement the ARMv5TE instruction set extensions and Coprocessor 15 (CP15) interface:
- **ARMv5TE Extensions**:
  - **`CLZ`**: Count Leading Zeros.
  - **`BLX <label/reg>`**: Branch with Link and Exchange (toggles Thumb state, handles register branch target states).
  - **Saturated Arithmetic**: `QADD`, `QSUB`, `QDADD`, `QDSUB` (modifies the `Q` bit 27 in CPSR on saturation).
  - **Enhanced Multiplies**: `SMULxy`, `SMLAxy`, `SMULWy`, `SMLAWy`, `SMLALxy` (signed multiplies operating on halfwords).
  - **Doubleword Access**: `LDRD` and `STRD` (64-bit load/stores to adjacent registers).
- **CP15 Coprocessor**:
  - Accessed via `MCR` and `MRC` instructions.
  - **Control Register (Reg 1)**: Configures DTCM/ITCM enables, instruction/data cache enables, and High Vectors (`0xFFFF0000`).
  - **TCM Control (Reg 9)**: Configures DTCM and ITCM base addresses and size fields.

---

## 2. NDS Memory Mapping (MMU)

The NDS MMU controls access to Main RAM, shared memory blocks, and I/O maps. Because ARM9 and ARM7 have different address mappings, the MMU exposes separate bus helpers.

### Memory Layout Map

| Region | Size | Target | ARM9 Access | ARM7 Access |
|---|---|---|---|---|
| `0x00000000` | 32KB / 64KB | ITCM (ARM9) / BIOS (ARM7) | ITCM (via CP15) | BIOS |
| `0x02000000` | 4MB | Main RAM | Yes (shared) | Yes (shared) |
| `0x03000000` | 32KB | Shared WRAM | Yes (configurable) | Yes (configurable) |
| `0x03800000` | 64KB | Private ARM7 WRAM | No | Yes |
| `0x04000000` | - | I/O Registers | ARM9 Ports | ARM7 Ports |
| `0x05000000` | 2KB / VRAM | Palette RAM | Yes (VRAM-mapped) | No |
| `0x06000000` | 656KB | VRAM Blocks | Yes (BG/OBJ/LCDC) | Yes (WRAM Banks C/D) |
| `0x07000000` | 2KB / VRAM | OAM (Sprite Attributes) | Yes (VRAM-mapped) | No |
| `0x08000000` | 128MB | Game Card / GBA Slot | Yes | Yes (shared) |
| `0x0B000000` | 16KB | DTCM (ARM9) | DTCM (via CP15) | No |

### Shared Work RAM (Shared WRAM)
The 32KB Shared WRAM is split into two 16KB blocks (Block 0 and Block 1) mapped under control of the `WRAMCNT` register (`0x04000241` on ARM9):
- **Mode 0**: Both blocks mapped to ARM9 at `0x03000000`.
- **Mode 1**: Both blocks mapped to ARM7 at `0x03800000` (appended to ARM7's private 64KB WRAM).
- **Mode 2**: Block 0 to ARM9 (`0x03000000`), Block 1 to ARM7 (`0x03800000`).
- **Mode 3**: Block 0 to ARM7 (`0x03800000`), Block 1 to ARM9 (`0x03000000`).

### VRAM Bank Mapping Control
VRAM consists of 9 distinct memory banks (A through I) that can be mapped to backgrounds, sprites, texture memory, ARM7 work RAM, or LCDC direct access:
- **Bank A, B, C, D**: 128KB each.
- **Bank E**: 64KB.
- **Bank F, G**: 16KB each.
- **Bank H**: 32KB.
- **Bank I**: 16KB.

Each bank has an 8-bit control register (`VRAMCNT_A` to `VRAMCNT_I`). Writing to a control register shifts the bank's address decoding:
- **MST (bits 0-2/3)**: Selects destination target (0: LCDC, 1: Main BG, 2: Main OBJ, 3: Main BG/OBJ/Texture, 4: Sub BG, 5: Sub OBJ, etc.).
- **Offset (bits 3-4)**: Determines the offset within the destination memory space (e.g., mapping multiple banks to form a contiguous 256KB or 512KB background region).
- **Enable (bit 7)**: Activates the mapping.

```rust
pub struct VramBank {
    pub data: Vec<u8>,
    pub control: u8,
}

pub struct Vram {
    pub banks: [VramBank; 9],
}

impl Vram {
    pub fn read_byte(&self, addr: u32, client: CpuClient) -> u8 {
        // Evaluate bank control registers to decode the address dynamically.
        // For example, if ARM9 reads 0x06000000 (Engine A BG VRAM), scan banks A-I
        // for MST == MainBG and map the address to the corresponding bank.
        // If LCDC mode is active, map address 0x06800000 + offset.
        // If ARM7 reads 0x06000000, map to Banks C/D if they are assigned to ARM7 WRAM.
        // ...
    }
}
```

---

## 3. IPC Registers and Interrupt Logic

The CPUs communicate via the `IPCSYNC` and `IPCFIFO` registers.

### IPCSYNC (0x04000180)
`IPCSYNC` is a 16-bit register allowing 4-bit message passing and IRQ signalling:
- **Bits 0-3**: Read-only, mirrors the other CPU's output bits.
- **Bits 8-11**: Read/Write, output status bits set by this CPU.
- **Bit 14**: Send IRQ (Write-only, self-clears). Writing a 1 triggers an `IPC_SYNC` interrupt on the other CPU (if enabled on the destination).
- **Bit 15**: Enable IRQ (Read/Write). If 1, this CPU will receive an interrupt when the other CPU sets its Send IRQ bit.

### IPCFIFO (0x04000184 Status, 0x04000188 Data)
The NDS has two unidirectional 16-word queues (each word is 32-bit):
1. **FIFO 9to7**: ARM9 writes (pushed), ARM7 reads (popped).
2. **FIFO 7to9**: ARM7 writes (pushed), ARM9 reads (popped).

#### FIFO Control Register (0x04000184):
- **Bit 0**: Send FIFO Empty.
- **Bit 1**: Send FIFO Full.
- **Bit 2**: Send FIFO Interrupt on Empty (triggers `IPC_SEND` IRQ locally when our send queue is empty).
- **Bit 3**: Send FIFO Clear (Write-only, flushes our send queue).
- **Bit 8**: Recv FIFO Empty.
- **Bit 9**: Recv FIFO Full.
- **Bit 10**: Recv FIFO Interrupt on Not Empty (triggers `IPC_RECV` IRQ locally when our receive queue contains data).
- **Bit 14**: FIFO Error (R/W, set on overflow/underflow, write 1 to clear).
- **Bit 15**: FIFO Enable (R/W, clear to disable FIFO).

#### Interrupt Evaluation Logic:
On any FIFO push or pop operation, check and assert/deassert the interrupt lines:

```rust
pub fn check_ipc_interrupts(&self, arm9_irq: &mut IrqControl, arm7_irq: &mut IrqControl) {
    // ARM9 Send (9to7) Empty IRQ
    if self.fifo_9to7.is_empty() && self.fifo_control_arm9.send_irq_on_empty {
        arm9_irq.trigger(Interrupt::IpcSend);
    }
    // ARM9 Recv (7to9) Not Empty IRQ
    if !self.fifo_7to9.is_empty() && self.fifo_control_arm9.recv_irq_on_not_empty {
        arm9_irq.trigger(Interrupt::IpcRecv);
    }

    // ARM7 Send (7to9) Empty IRQ
    if self.fifo_7to9.is_empty() && self.fifo_control_arm7.send_irq_on_empty {
        arm7_irq.trigger(Interrupt::IpcSend);
    }
    // ARM7 Recv (9to7) Not Empty IRQ
    if !self.fifo_9to7.is_empty() && self.fifo_control_arm7.recv_irq_on_not_empty {
        arm7_irq.trigger(Interrupt::IpcRecv);
    }
}
```

---

## 4. HLE Direct ROM Booting Setup

For High-Level Emulation, the bootloader bypasses the NDS BIOS screen and loads the game binary directly into memory, priming both CPUs for immediate execution.

### ROM Header Layout (0x0200 bytes)

- `0x000` - 12 bytes: **Game Title** (ASCII).
- `0x00C` - 4 bytes: **Game Code** (ASCII, e.g. `IPGE` for Pokémon SoulSilver).
- `0x020` - 4 bytes: **ARM9 ROM Offset** (source location in ROM).
- `0x024` - 4 bytes: **ARM9 Entry Address** (initial ARM9 Program Counter).
- `0x028` - 4 bytes: **ARM9 RAM Address** (load destination in memory).
- `0x02C` - 4 bytes: **ARM9 Size** (size of ARM9 binary).
- `0x030` - 4 bytes: **ARM7 ROM Offset** (source location in ROM).
- `0x034` - 4 bytes: **ARM7 Entry Address** (initial ARM7 Program Counter).
- `0x038` - 4 bytes: **ARM7 RAM Address** (load destination in memory).
- `0x03C` - 4 bytes: **ARM7 Size** (size of ARM7 binary).
- `0x074` - 4 bytes: **ARM9 Autoload Info Offset**.
- `0x078` - 4 bytes: **ARM7 Autoload Info Offset**.

### HLE Boot Procedure

1. **Header Parsing**: Validate ROM size and parse the ROM header fields.
2. **Binary Copying**:
   - Copy ARM9 binary from `arm9_rom_offset` of size `arm9_size` to `arm9_ram_address` in Main RAM.
   - Copy ARM7 binary from `arm7_rom_offset` of size `arm7_size` to `arm7_ram_address` in Main RAM or private WRAM.
3. **ROM Header Copy**:
   - Copy the first `0x160` (or `0x200`) bytes of the ROM header to `0x027FFE00` in Main RAM. This allows runtime checks by the game engine.
4. **Autoload Copy**:
   - Inspect the autoload pointers (from offsets `0x074` and `0x078`).
   - If present, parse the autoload tables (ITCM/DTCM sizes and load addresses) and copy the corresponding sections from RAM into the ITCM (`0x01000000`) and DTCM (`0x0B000000`) arrays before starting execution.
5. **CPU Initialization Registers**:
   - **ARM9**:
     - `CPSR` = `0x000000D3` (Supervisor Mode, ARM state, IRQs disabled) or `0x000000DF` (System Mode).
     - `R0` - `R11` = `0`.
     - `R12` = `arm9_entry_address`.
     - `R13 (SP)` = `0x03002F00` (System WRAM stack) or configured base in DTCM.
     - `R14 (LR)` = `arm9_entry_address`.
     - `R15 (PC)` = `arm9_entry_address`.
     - CP15 Control: DTCM/ITCM enabled. DTCM mapped at `0x027C0000` / `0x0B000000`, ITCM mapped at `0x01000000`.
   - **ARM7**:
     - `CPSR` = `0x000000DF` (System Mode, ARM state, IRQs disabled).
     - `R0` - `R11` = `0`.
     - `R12` = `arm7_entry_address`.
     - `R13 (SP)` = `0x0380FFFC` or `0x0380FD00` (ARM7 private WRAM stack).
     - `R14 (LR)` = `arm7_entry_address`.
     - `R15 (PC)` = `arm7_entry_address`.
6. **Instruction Pipeline Priming**:
   - Flush and refill the instruction pipelines for both CPUs to match the execution invariants.
   - Read 2 instructions from each entry point:
     - `pipeline[0] = read_word(entry_point)`
     - `pipeline[1] = read_word(entry_point + 4)`
     - Set R15 (PC) to `entry_point + 8` (ARM state).
7. **Boot Memory Flags**:
   - Write BIOS boot complete indicators to RAM:
     - `0x027FFFC0` = `0x01` (ARM9 boot status) or `0x66` (Booted).
     - `0x027FFFC4` = `0x01` (ARM7 boot status) or `0x66` (Booted).
     - `0x027FFFC8` = `0` (Lid open).
     - `0x027FFFCC` = `0` (Boot indicator).
