//! Text preview of every mode through the REAL UI draw path
//! (`ui::snapshot`), for a quick look without a terminal. For pixel-level
//! inspection use `--example snapshot`, which rasterizes cells to a BMP.
//!
//! cargo run --example preview [-- WIDTHxHEIGHT]

fn main() {
    let (w, h) = std::env::args()
        .nth(1)
        .and_then(|s| {
            let (a, b) = s.split_once('x')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or((72u16, 20u16));
    for mode in 1..=6u8 {
        let buf = spectrum::ui::snapshot(&spectrum::ui::Snapshot {
            mode,
            theme: "spectrum",
            depth: spectrum::theme::ColorDepth::True,
            at: 4.5,
            frozen: false,
            warmup: 1.5,
            lyric: None,
            width: w,
            height: h,
        });
        println!("--- mode {mode} @ {w}x{h} ---");
        for y in 0..h {
            let row: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
            println!("|{row}|");
        }
        println!();
    }
}
