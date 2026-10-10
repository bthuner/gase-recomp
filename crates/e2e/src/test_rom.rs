// A tiny Mega Drive ROM, assembled by hand (shared by build.rs and tests).
//
// No game ships with gase-recomp, so we write our own: a few hundred bytes
// of 68000 machine code. It is also a compact illustration of what a
// minimal Mega Drive program must do:
//
//   1. vector table + Sega header
//   2. unlock TMSS, set up the VDP registers
//   3. upload a palette, two tiles and a tilemap
//   4. enable the display and the VBlank interrupt
//   5. main loop: wait for VBlank, then do some work
//
// The work exercises the hard parts of static recompilation: a jump table
// (`jmp table(pc,d0.w)` + BRA.W), an indirect call the analysis cannot see
// (`jsr (a0)`), and an interrupt handler driving the main loop.
//
// RAM results:  $FF0000.b vblank flag   $FF0004-7.b jump-table counters
//               $FF0002.w frame counter $FF0008.b   indirect-call counter
//               $FF0010.l vblank count  $FF0009.b   static-call counter

#[allow(dead_code)]
mod test_rom {
    use std::collections::HashMap;

    pub const FLAG: u32 = 0xFF0000;
    pub const FRAMES: u32 = 0xFF0002;
    pub const JT: u32 = 0xFF0004;
    pub const ICALL: u32 = 0xFF0008;
    pub const SCALL: u32 = 0xFF0009;
    pub const VBLS: u32 = 0xFF0010;
    /// The routine only reachable through `jsr (a0)`.
    pub const INDIRECT_SUB: u32 = 0x2EE;

    enum Item {
        Word(u16),
        Rel16(&'static str, u32),
        Rel8(&'static str, u32, u16),
        PcIndex(&'static str, u32),
    }

    /// A two-pass mini assembler: emit words, resolve labels at the end.
    struct Asm {
        items: Vec<Item>,
        labels: HashMap<&'static str, u32>,
        pc: u32,
    }

    impl Asm {
        fn label(&mut self, name: &'static str) {
            self.labels.insert(name, self.pc);
        }
        fn w(&mut self, words: &[u16]) {
            for &x in words {
                self.items.push(Item::Word(x));
                self.pc += 2;
            }
        }
        fn l(&mut self, v: u32) {
            self.w(&[(v >> 16) as u16, v as u16]);
        }
        /// 16-bit displacement to `name`, relative to this extension word.
        fn rel16(&mut self, name: &'static str) {
            self.items.push(Item::Rel16(name, self.pc));
            self.pc += 2;
        }
        /// Short branch: displacement in the opcode's low byte.
        fn rel8(&mut self, opcode: u16, name: &'static str) {
            self.items.push(Item::Rel8(name, self.pc, opcode));
            self.pc += 2;
        }
        /// Brief extension word for `d8(pc,d0.w)`.
        fn pcindex(&mut self, name: &'static str) {
            self.items.push(Item::PcIndex(name, self.pc));
            self.pc += 2;
        }
        fn assemble(&self) -> Vec<u8> {
            let mut out = Vec::new();
            for it in &self.items {
                let word = match *it {
                    Item::Word(x) => x,
                    Item::Rel16(n, at) => self.labels[n].wrapping_sub(at) as u16,
                    Item::Rel8(n, at, op) => op | (self.labels[n].wrapping_sub(at + 2) as u16 & 0xFF),
                    Item::PcIndex(n, at) => self.labels[n].wrapping_sub(at) as u16 & 0xFF,
                };
                out.extend(word.to_be_bytes());
            }
            out
        }
    }

    /// The VDP command "write VRAM at addr".
    fn vram_write(addr: u32) -> u32 {
        0x4000_0000 | ((addr & 0x3FFF) << 16) | (addr >> 14)
    }

    pub fn build() -> Vec<u8> {
        let mut a = Asm { items: Vec::new(), labels: HashMap::new(), pc: 0x200 };
        a.label("reset");
        a.w(&[0x46FC, 0x2700]); //               move.w #$2700,sr
        a.w(&[0x1039]); a.l(0xA10001); //        move.b $A10001,d0
        a.w(&[0x0200, 0x000F]); //               andi.b #$0F,d0
        a.rel8(0x6700, "no_tmss"); //            beq.s no_tmss
        a.w(&[0x23FC]); a.l(0x5345_4741); a.l(0xA14000); // move.l #'SEGA',$A14000
        a.label("no_tmss");
        a.w(&[0x43F9]); a.l(0xC00004); //        lea $C00004,a1  (VDP control)
        a.w(&[0x45F9]); a.l(0xC00000); //        lea $C00000,a2  (VDP data)
        a.w(&[0x41FA]); a.rel16("vdp_regs"); //  lea vdp_regs(pc),a0
        a.w(&[0x7200 | 18]); //                  moveq #18,d1
        a.label("reg_loop");
        a.w(&[0x3298]); //                       move.w (a0)+,(a1)
        a.w(&[0x51C9]); a.rel16("reg_loop"); //  dbra d1,reg_loop
        a.w(&[0x22BC]); a.l(0xC000_0000); //     move.l #CRAM write 0,(a1)
        a.w(&[0x41FA]); a.rel16("palette");
        a.w(&[0x7200 | 15]);
        a.label("pal_loop");
        a.w(&[0x3498]); //                       move.w (a0)+,(a2)
        a.w(&[0x51C9]); a.rel16("pal_loop");
        a.w(&[0x22BC]); a.l(vram_write(0x20)); // tiles 1 and 2
        a.w(&[0x41FA]); a.rel16("tiles");
        a.w(&[0x7200 | 15]);
        a.label("tile_loop");
        a.w(&[0x2498]); //                       move.l (a0)+,(a2)
        a.w(&[0x51C9]); a.rel16("tile_loop");
        for (row, tile, lbl) in [(10u32, 1u16, "map10"), (12, 2, "map12")] {
            a.w(&[0x22BC]); a.l(vram_write(0xC000 + row * 128));
            a.w(&[0x7400 | tile]); //            moveq #tile,d2
            a.w(&[0x7200 | 39]); //              moveq #39,d1
            a.label(lbl);
            a.w(&[0x3482]); //                   move.w d2,(a2)
            a.w(&[0x51C9]); a.rel16(lbl);
        }
        a.w(&[0x32BC, 0x8174]); //               display on, VBlank IRQ on
        a.w(&[0x46FC, 0x2000]); //               move.w #$2000,sr

        a.label("main");
        a.w(&[0x4A39]); a.l(FLAG); //            tst.b flag
        a.rel8(0x6700, "main"); //               beq.s main
        a.w(&[0x4239]); a.l(FLAG); //            clr.b flag
        a.w(&[0x5279]); a.l(FRAMES); //          addq.w #1,frames
        a.w(&[0x6100]); a.rel16("static_sub"); // bsr.w static_sub
        a.w(&[0x41FA]); a.rel16("indirect_sub"); // lea indirect_sub(pc),a0
        a.w(&[0x4E90]); //                       jsr (a0)
        a.w(&[0x3039]); a.l(FRAMES); //          move.w frames,d0
        a.w(&[0x0240, 0x0003]); //               andi.w #3,d0
        a.w(&[0xD040, 0xD040]); //               add.w d0,d0 (x2)
        a.w(&[0x4EFB]); a.pcindex("table"); //   jmp table(pc,d0.w)
        a.label("table");
        for c in ["case0", "case1", "case2", "case3"] {
            a.w(&[0x6000]); a.rel16(c); //       bra.w caseN
        }
        for (n, c) in ["case0", "case1", "case2", "case3"].into_iter().enumerate() {
            a.label(c);
            a.w(&[0x5239]); a.l(JT + n as u32); // addq.b #1,counter[n]
            a.w(&[0x6000]); a.rel16("main"); //  bra.w main
        }
        a.label("static_sub");
        a.w(&[0x5239]); a.l(SCALL); //           addq.b #1,$FF0009
        a.w(&[0x4E75]); //                       rts
        a.label("indirect_sub"); //              only reachable via jsr (a0)
        a.w(&[0x3439]); a.l(FRAMES); //          move.w frames,d2
        a.w(&[0x0242, 0x000F]); //               andi.w #$F,d2
        a.w(&[0x0042, 0x8700]); //               ori.w #$8700,d2 (backdrop colour)
        a.w(&[0x3282]); //                       move.w d2,(a1)
        a.w(&[0x5239]); a.l(ICALL); //           addq.b #1,$FF0008
        a.w(&[0x4E75]); //                       rts
        a.label("vblank");
        a.w(&[0x13FC, 0x0001]); a.l(FLAG); //    move.b #1,flag
        a.w(&[0x52B9]); a.l(VBLS); //            addq.l #1,vblanks
        a.w(&[0x4E73]); //                       rte
        a.label("error"); //                     crash handler: spin
        a.w(&[0x4E71]);
        a.rel8(0x6000, "error");
        a.label("vdp_regs");
        a.w(&[0x8004, 0x8134, 0x8230, 0x8328, 0x8407, 0x855C, 0x8600, 0x8700, 0x8800, 0x8900,
              0x8AFF, 0x8B00, 0x8C81, 0x8D2E, 0x8E00, 0x8F02, 0x9001, 0x9100, 0x9200]);
        a.label("palette");
        a.w(&[0x0000, 0x000E, 0x00E0, 0x0E00]);
        a.w(&[0x0EEE; 12]);
        a.label("tiles");
        for _ in 0..8 {
            a.l(0x1111_1111); //                 tile 1: solid colour 1 (red)
        }
        for row in 0..8 {
            a.l(if row % 2 == 0 { 0x2323_2323 } else { 0x3232_3232 }); // tile 2: checkerboard
        }
        assert_eq!(a.labels["indirect_sub"], INDIRECT_SUB);

        let mut rom = vec![0u8; 0x200];
        rom.extend(a.assemble());
        rom.resize(rom.len().next_multiple_of(0x400), 0xFF);
        let put32 = |rom: &mut Vec<u8>, at: usize, v: u32| rom[at..at + 4].copy_from_slice(&v.to_be_bytes());
        put32(&mut rom, 0, 0x00FF_FE00); //      initial SSP
        for v in 1..64 {
            put32(&mut rom, v * 4, a.labels["error"]);
        }
        put32(&mut rom, 4, a.labels["reset"]);
        put32(&mut rom, 30 * 4, a.labels["vblank"]);
        for (at, text) in [(0x100, &b"SEGA MEGA DRIVE "[..]), (0x110, b"(C)GASE 2026.OCT"),
                           (0x120, b"GASE TEST ROM"), (0x150, b"GASE TEST ROM"),
                           (0x180, b"GM 00000000-00"), (0x190, b"J"), (0x1F0, b"JUE")] {
            rom[at..at + text.len()].copy_from_slice(text);
            if at == 0x120 || at == 0x150 {
                rom[at + text.len()..at + 48].fill(b' ');
            }
        }
        let len = rom.len() as u32;
        put32(&mut rom, 0x1A0, 0);
        put32(&mut rom, 0x1A4, len - 1);
        put32(&mut rom, 0x1A8, 0xFF0000);
        put32(&mut rom, 0x1AC, 0xFFFFFF);
        let checksum = rom[0x200..].chunks(2).fold(0u16, |s, w| s.wrapping_add(u16::from_be_bytes([w[0], w[1]])));
        rom[0x18E..0x190].copy_from_slice(&checksum.to_be_bytes());
        rom
    }
}
