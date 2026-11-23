```
 ▄▄                    ▄▄
 ██ ▀▀▀█▄   ▄▄▄▄▄▄▄▄▄▄ ██   ▄▄▄▄
 ████████ ▄▄███████████▀▀  ▄▄████
 ███████████████████████████████▀
```

spectrum is a terminal audio visualizer written in rust. play a local audio file or capture whatever your computer is playing and watch it turn into real time fft visualizations drawn with unicode blocks, braille dots and ansi color.

how to run on windows. download the zip from the releases page, extract it anywhere and double tap on spectrum-app.exe. thats it. youll get a choise, pick 1 for the own terminal wich opens fullscreen with a readable menu and the best experiance, or pick 2 for the normal terminal to run in whatever console you already have open, just know the lyrics garden view may glitch out there.

on linux you build from source. you need rust installed, get it from rustup.rs if you dont have it. then clone https://github.com/neticytof/spectrum.git, go into the folder, run cargo build --release and start it with ./target/release/spectrum.

the lyrics garden needs an lrc file. you have to place an identically named .lrc file next to your music, in the same folder. so if your song is song.mp3 you need song.lrc beside it. the lrc file is just plain text with timestamps like [00:01.00] first line of the song and [00:05.50] second line. no lrc file means no lyrics, the app wont guess or make anything up, itll just tell you the sidecar is missing. if the lrc file has no timestamps you get plain unsynced text instead of the garden.

there are 5 visual modes and you switch between them anytime with keys 1 to 5. mode 1 is spectrum, frequency bars with anti aliased tops and peak caps that fall down. mode 2 is oscilloscope, a waveform trace that holds its shape using phase triggering. mode 3 is radial, spectrum petals arranged in a circle around a breathing ring. mode 4 is mirror and its the default, bass in the middle mirrored outward like a butterfly. mode 5 is the lyrics garden, the current lyric line rendered as big readable serif letters with flowers and stems growing out of them, driven by the lrc timestamps.

the controls are simple. press 1 to 5 to switch modes, space pauses or resumes, m mutes or unmutes, plus and minus turn the volume up and down, arrow keys seek forward or back, q or esc quits.

the header has a glowing dot that pulses when the music hits. thats the beat detection, driven by spectral flux, wich is a fancy way of saying it watches for sudden changes in the frequency spectrum.

for audio sources you can pick any local file (mp3, wav, ogg or flac) or capture live from what windows is playing right now (system loopback) or your microphone.

there are expert flags too, any command line flag bypasses the menu entirely. --theme picks a color theme (spectrum, fire, ice, matrix, vapor), --mode starts in a mode from 1 to 5, --colors picks true, 256 or 16 color depth, --bars sets the spectrum bands from 8 to 256, --sensitivity sets the input gain, --smoothing sets the bar release speed, --volume sets playback volume from 0.0 to 1.0, --source picks system or mic for live capture, --demo runs a silent synthetic demo, --demo-lyric shows custom text in the garden demo, --freeze-at freezes the demo timeline at a point, --exit-after auto quits after some seconds.

how it was all achieved. the whole thing is written in rust, the analysis runs on real audio samples through an fft, and the drawing is unicode and ansi color straight into the terminal. the lyrics garden is a port of ascii garden by ridit (mit license), it renders the genuine playfair display font (sil ofl 1.1) embedded in the binary and rasterized with ab_glyph, so the letterforms are actual serif shapes and not blocky ascii art, and the flowers grow out of the letters inside a strict budget so every letter stays readable. the own terminal is the official portable windows terminal runtime (mit) with the cascadia mono font (sil ofl 1.1), it keeps all its settings in its own folder next to the app and never touches your global terminal preferences. the project itself is mit licensed, see the license file, and the notice file has the details for the fonts and the terminal runtime.

building from source for development is the same cargo build, and cargo test runs the tests.

this project was create 100% by ai although i have put in my own efforts in this and this is not slop nor is it one shot.

