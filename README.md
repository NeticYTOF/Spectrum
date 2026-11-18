# SPECTRUM

A terminal audio visualizer written in Rust. Play a local audio file or capture live audio and watch it as real time FFT visualizations rendered with Unicode blocks braille dots and ANSI color.

```
 ▄▄                    ▄▄
 ██ ▀▀▀█▄   ▄▄▄▄▄▄▄▄▄▄ ██   ▄▄▄▄
 ████████ ▄▄███████████▀▀  ▄▄████
 ███████████████████████████████▀
```

## How to run

### Windows

Just double tap on `spectrum-app.exe`. Thats it.

Youll get a choise: pick `1) Own terminal` for the best experiance or `2) Normal terminal` to run in whatever terminal you already have open.

The own terminal opens fullscreen with a readable menu. Pick what you want from the numbered options and it just works.

### Linux

Build from source:

```bash
git clone https://github.com/NeticYTOF/Spectrum.git
cd Spectrum
cargo build --release
./target/release/spectrum
```

You need Rust installed. Get it from rustup.rs if you dont have it.

## The lyrics garden needs an LRC file

For the lyrics garden to work you need a `.lrc` file with the same name as your music file in the same folder. So if your song is `song.mp3` you need `song.lrc` next to it.

The LRC file is just text with timestamps like this:

```
[00:01.00]First line of the song
[00:05.50]Second line here
[00:10.25]Third line
```

No LRC file means no lyrics. The app wont guess or make anything up, itll just tell you the sidecar is missing. If the LRC file has no timestamps you get plain unsynced text instead of the garden.

## All the visual modes

Press keys 1 through 5 to switch between them anytime

- **1 Spectrum** - frequency bars with anti aliased tops and peak caps that fall down
- **2 Oscilloscope** - a waveform trace that holds its shape using phase triggering
- **3 Radial** - spectrum petals arranged in a circle around a breathing ring
- **4 Mirror** (default) - bass in the middle mirrored outward like a butterfly
- **5 Lyrics garden** - the current lyric line rendered as serif letters with flowers and stems growing out of them. Driven by the LRC sidecar timestamps.

## Controls

- `1` to `5` - switch visual mode
- `space` - pause or resume
- `m` - mute or unmute
- `+` and `-` - volume up and down
- `q` or `esc` - quit
- `arrow keys` - seek forward or back

## Beat detection

The header has a glowing dot that pulses when the music hits. Its driven by spectral flux which is just a fancy way of saying it watches for sudden changes in the frequency spectrum.

## Audio sources

- **Local file** - pick any mp3 wav ogg or flac
- **Live capture** - what Windows is playing right now (system loopback) or your microphone

## What the lyrics garden looks like

When a synced LRC file is present the current line appears as large readable serif letters with flowers and curling stems growing out of them. The letters reveal line by line as the song plays. Its inspired by ascii garden and built with a real font rasterizer so the letterforms are actual Playfair Display shapes not just blocky ASCII art.

The garden needs a big terminal to look its best. At very small terminal sizes it falls back to plain text so you can still read the words.

## Expert flags

Any command line flag bypasses the menu entirely

```
spectrum [options] [audio file]

--theme NAME        color theme (spectrum fire ice matrix vapor)
--mode N            start in mode 1-5
--colors DEPTH      true 256 or 16 color depth
--bars N            spectrum bands 8-256
--sensitivity F     input gain
--smoothing F       bar release speed
--volume F          playback volume 0.0 to 1.0
--source TYPE       system or mic for live capture
--demo              silent synthetic demo mode
--demo-lyric TEXT   show this text in the lyrics garden demo
--freeze-at SECS    freeze the demo timeline at this point
--exit-after SECS   auto quit after this many seconds
```

## Building from source

```bash
cargo build --release
```

For development with tests:

```bash
cargo test
```

The lyrics garden uses an embedded Playfair Display font (SIL OFL 1.1 license) rasterized with ab_glyph. The own terminal feature uses a portable Windows Terminal runtime (MIT license) with Cascadia Mono font (SIL OFL 1.1). All licenses are in the `terminal/` folder.

## License

MIT. See LICENSE file.
