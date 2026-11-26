//! Draws the tray icon: memory use in percent on a colour that follows the warning levels.

const FONT: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

pub const SIZE: u32 = 32;

pub fn render(pct: u32, warn: u32, danger: u32) -> Vec<u8> {
    let bg: [u8; 3] = if pct >= danger {
        [0xC6, 0x28, 0x28]
    } else if pct >= warn {
        [0xC7, 0x7A, 0x0A]
    } else {
        [0x0F, 0x7B, 0x5F]
    };
    let n = SIZE as usize;
    let mut px = vec![0u8; n * n * 4];
    // Rounded square background (radius 5).
    for y in 0..n {
        for x in 0..n {
            let r = 5i32;
            let cx = (x as i32).clamp(r, n as i32 - 1 - r);
            let cy = (y as i32).clamp(r, n as i32 - 1 - r);
            let (dx, dy) = (x as i32 - cx, y as i32 - cy);
            if dx * dx + dy * dy <= r * r {
                let i = (y * n + x) * 4;
                px[i..i + 4].copy_from_slice(&[bg[0], bg[1], bg[2], 255]);
            }
        }
    }
    let v = pct.min(99);
    let digits = [(v / 10) as usize, (v % 10) as usize];
    // Each font pixel becomes a 3x4 block: two digits fill 32x28.
    for (d, &digit) in digits.iter().enumerate() {
        let ox = if d == 0 { 0 } else { 17 };
        for (row, bits) in FONT[digit].iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                for by in 0..4 {
                    for bx in 0..3 {
                        let x = ox + col * 3 + bx;
                        let y = 2 + row * 4 + by;
                        if x < n && y < n {
                            let i = (y * n + x) * 4;
                            px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                        }
                    }
                }
            }
        }
    }
    px
}
