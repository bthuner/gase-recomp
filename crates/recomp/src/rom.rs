//! Load a Mega Drive ROM and read its header.
//!
//! A cartridge ROM is the bytes the 68000 sees at `$000000-$3FFFFF`. Its
//! first 512 bytes are special:
//!
//! * `$000-$0FF`: the exception vector table, 64 big-endian addresses
//!   (`$000` initial stack pointer, `$004` reset entry point, `$070`
//!   level 4 = HBlank, `$078` level 6 = VBlank, ...);
//! * `$100-$1FF`: the Sega header (console name, titles, serial, checksum,
//!   region codes...).
//!
//! Dumps come flat (`.bin`, `.gen`, `.md`) or in the "Super Magic Drive"
//! copier format (`.smd`): a 512-byte header, then 16 KiB blocks whose
//! first half holds the odd bytes and second half the even bytes. We
//! de-interleave those back to a flat image.

use std::fmt;

/// Names of the useful exception vectors.
#[must_use]
pub fn vector_name(n: usize) -> String {
    match n {
        0 => "initial_ssp".into(),
        1 => "reset".into(),
        2 => "bus_error".into(),
        3 => "address_error".into(),
        4 => "illegal_instruction".into(),
        5 => "zero_divide".into(),
        6 => "chk".into(),
        7 => "trapv".into(),
        8 => "privilege_violation".into(),
        9 => "trace".into(),
        10 => "line_a".into(),
        11 => "line_f".into(),
        24 => "spurious_irq".into(),
        28 => "irq4_hblank".into(),
        30 => "irq6_vblank".into(),
        25..=31 => format!("irq{}", n - 24),
        32..=47 => format!("trap{}", n - 32),
        _ => format!("vector{n}"),
    }
}

#[derive(Debug)]
pub struct RomError(pub String);

impl fmt::Display for RomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RomError {}

#[derive(Clone, Debug)]
pub struct Header {
    pub console: String,
    pub title_domestic: String,
    pub title_overseas: String,
    pub serial: String,
    pub checksum: u16,
    pub region: String,
}

#[derive(Clone, Debug)]
pub struct Rom {
    /// Flat, big-endian ROM image.
    pub data: Vec<u8>,
    pub header: Header,
    pub vectors: [u32; 64],
    pub smd: bool,
}

fn text(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .trim_matches(|c: char| c == ' ' || c == '\0')
        .to_string()
}

fn looks_like_smd(raw: &[u8]) -> bool {
    if raw.len() < 0x200 + 0x4000 || (raw.len() - 0x200) % 0x4000 != 0 {
        return false;
    }
    if raw[8] == 0xAA && raw[9] == 0xBB {
        return true;
    }
    &raw[0x100..0x104] != b"SEGA" && &deinterleave(raw)[0x100..0x104] == b"SEGA"
}

/// Undo the SMD block interleave (odd bytes first, then even bytes).
fn deinterleave(raw: &[u8]) -> Vec<u8> {
    let body = &raw[0x200..];
    let mut out = vec![0; body.len()];
    for (b, chunk) in body.chunks(0x4000).enumerate() {
        let (odd, even) = chunk.split_at(0x2000);
        for i in 0..0x2000 {
            out[b * 0x4000 + 2 * i] = even[i];
            out[b * 0x4000 + 2 * i + 1] = odd[i];
        }
    }
    out
}

impl Rom {
    /// Parse a ROM image (flat or SMD).
    pub fn from_bytes(raw: &[u8]) -> Result<Self, RomError> {
        let smd = looks_like_smd(raw);
        let mut data = if smd { deinterleave(raw) } else { raw.to_vec() };
        if data.len() < 0x200 {
            return Err(RomError(
                "file too small to be a Mega Drive ROM (no header)".into(),
            ));
        }
        if data.len() > 0x40_0000 {
            return Err(RomError(
                "ROMs above 4 MiB (SSF2 mapper) are not supported yet".into(),
            ));
        }
        if data.len() % 2 == 1 {
            data.push(0xFF);
        }
        let mut vectors = [0; 64];
        for (i, v) in vectors.iter_mut().enumerate() {
            *v = u32::from_be_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let h = &data[0x100..0x200];
        let header = Header {
            console: text(&h[0x00..0x10]),
            title_domestic: text(&h[0x20..0x50]),
            title_overseas: text(&h[0x50..0x80]),
            serial: text(&h[0x80..0x8E]),
            checksum: u16::from_be_bytes([h[0x8E], h[0x8F]]),
            region: text(&h[0xF0..0xF3]),
        };
        Ok(Self {
            data,
            header,
            vectors,
            smd,
        })
    }

    #[must_use]
    pub fn u16(&self, addr: u32) -> Option<u16> {
        let a = addr as usize;
        (a + 1 < self.data.len()).then(|| u16::from_be_bytes([self.data[a], self.data[a + 1]]))
    }

    #[must_use]
    pub fn title(&self) -> &str {
        [&self.header.title_overseas, &self.header.title_domestic]
            .into_iter()
            .find(|t| !t.is_empty())
            .map_or("UNTITLED", String::as_str)
    }

    /// Sum of the big-endian words from `$200` to the end, modulo 2^16.
    #[must_use]
    pub fn computed_checksum(&self) -> u16 {
        self.data[0x200..]
            .chunks(2)
            .map(|w| u16::from_be_bytes([w[0], *w.get(1).unwrap_or(&0)]))
            .fold(0u16, u16::wrapping_add)
    }

    #[must_use]
    pub fn crc32(&self) -> u32 {
        crc32(&self.data)
    }
}

/// The CRC-32 used by zip and PNG: the generated code checks it at start-up
/// to make sure it runs with the ROM it was recompiled from.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}
