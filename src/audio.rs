use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use rodio::{Decoder, OutputStream, Sink, Source};

/// Maximum samples buffered between the audio producer and the analyzer.
/// Keeps latency bounded; overflow silently drops the oldest samples.
pub const QUEUE_CAP: usize = 16_384;

pub type SampleQueue = Arc<Mutex<VecDeque<f32>>>;

pub fn new_queue() -> SampleQueue {
    Arc::new(Mutex::new(VecDeque::with_capacity(QUEUE_CAP)))
}

/// Push one mono sample to the analysis queue.
///
/// Locks are short and never panic: a poisoned lock is recovered from
/// instead of taking down the audio callback.
pub fn push_samples(queue: &SampleQueue, samples: &[f32]) {
    if samples.is_empty() {
        return;
    }
    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
    for &s in samples {
        if s.is_finite() {
            q.push_back(s);
        }
    }
    while q.len() > QUEUE_CAP {
        q.pop_front();
    }
}

#[derive(Clone)]
pub struct AudioMeta {
    pub sample_rate: u32,
    pub description: String,
    /// Honest capture status shown in the UI ("ok" / "reconnecting" /
    /// the last device error).
    pub status: &'static str,
}

/// Which live source to capture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptureSource {
    /// Whatever Windows is outputting (WASAPI loopback on the default
    /// output device).
    System,
    /// The default recording device.
    Microphone,
}

impl CaptureSource {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "system" | "output" | "loopback" => Some(CaptureSource::System),
            "mic" | "microphone" | "input" => Some(CaptureSource::Microphone),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CaptureSource::System => "system output",
            CaptureSource::Microphone => "microphone",
        }
    }
}

/// UI -> capture-owner commands.
#[derive(Clone, Copy)]
enum CapCmd {
    Pause,
    Play,
}

/// State shared between the UI thread and the capture owner: the command
/// channel plus honest status. The `cpal::Stream` itself never appears here
/// (or anywhere shared): cpal marks `Stream` `!Send` on every platform, so
/// the stream is created, used and dropped on the owner thread alone.
struct CaptureShared {
    tx: Sender<CapCmd>,
    device_name: String,
    paused: bool,
    /// Set when a device change was detected but the new stream could not
    /// be opened; cleared on the next successful reopen.
    last_error: Option<String>,
}

/// Running audio source plus the handles needed to control it.
pub struct AudioHandle {
    pub meta: Arc<Mutex<AudioMeta>>,
    pub sink: Option<Sink>,
    capture: Option<Arc<Mutex<CaptureShared>>>,
    /// Must outlive `sink`; dropping it stops all playback.
    _output: Option<OutputStream>,
    /// Demo mode: no audio hardware is touched at all.
    pub demo: bool,
    pub demo_pause: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl AudioHandle {
    pub fn is_live(&self) -> bool {
        self.sink.is_none()
    }

    pub fn pause(&self) {
        if self.demo {
            if let Some(p) = &self.demo_pause {
                p.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            return;
        }
        if let Some(sink) = &self.sink {
            sink.pause();
        } else if let Some(cap) = &self.capture {
            let mut shared = cap.lock().unwrap_or_else(|e| e.into_inner());
            shared.paused = true;
            let _ = shared.tx.send(CapCmd::Pause);
        }
    }

    pub fn resume(&self) {
        if self.demo {
            if let Some(p) = &self.demo_pause {
                p.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            return;
        }
        if let Some(sink) = &self.sink {
            sink.play();
        } else if let Some(cap) = &self.capture {
            let mut shared = cap.lock().unwrap_or_else(|e| e.into_inner());
            shared.paused = false;
            let _ = shared.tx.send(CapCmd::Play);
        }
    }

    /// Honest pause state (capture tracks it explicitly since cpal offers
    /// no query).
    pub fn is_paused(&self) -> bool {
        if self.demo {
            return self
                .demo_pause
                .as_ref()
                .map(|p| p.load(std::sync::atomic::Ordering::Relaxed))
                .unwrap_or(false);
        }
        if let Some(cap) = &self.capture {
            return cap.lock().unwrap_or_else(|e| e.into_inner()).paused;
        }
        false
    }

    /// True when file playback has run to the end.
    pub fn is_finished(&self) -> bool {
        self.sink.as_ref().map(|s| s.empty()).unwrap_or(false)
    }

    pub fn volume(&self) -> f32 {
        self.sink.as_ref().map(|s| s.volume()).unwrap_or(0.0)
    }

    pub fn set_volume(&self, v: f32) {
        if let Some(sink) = &self.sink {
            sink.set_volume(v.clamp(0.0, 1.0));
        }
    }

    /// A demo handle: no devices, no threads, nothing to pause but the
    /// synthetic generator itself.
    pub fn for_demo() -> Self {
        Self {
            meta: Arc::new(Mutex::new(AudioMeta {
                sample_rate: 44_100,
                description: "synthetic \u{b7} silent".into(),
                status: "demo",
            })),
            sink: None,
            capture: None,
            _output: None,
            demo: true,
            demo_pause: Some(Arc::new(std::sync::atomic::AtomicBool::new(false))),
        }
    }

    /// Human-readable meta snapshot for the status bar.
    pub fn meta_snapshot(&self) -> AudioMeta {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Wraps a rodio source and taps the samples actually pulled by the
/// playback engine, mixing them down to mono for the analyzer. This
/// guarantees the visuals are exactly in sync with what is audible.
///
/// Tapped samples are batched and flushed to the queue in bulk so the
/// audio thread takes the queue lock ~170 times per second instead of
/// once per sample (~44,000).
const TEE_BATCH: usize = 512;

pub struct TeeSource<S> {
    source: S,
    queue: SampleQueue,
    channels: u16,
    acc: f32,
    count: u16,
    batch: Vec<f32>,
}

impl<S> TeeSource<S> {
    pub fn new(source: S, channels: u16, queue: SampleQueue) -> Self {
        Self {
            source,
            queue,
            channels: channels.max(1),
            acc: 0.0,
            count: 0,
            batch: Vec::with_capacity(TEE_BATCH),
        }
    }

    /// Bulk-push the pending batch under a single lock acquisition.
    fn flush(&mut self) {
        if !self.batch.is_empty() {
            push_samples(&self.queue, &self.batch);
            self.batch.clear();
        }
    }
}

/// Whatever the decoder or capture device yields becomes f32 for analysis.
/// Covers every rodio/cpal sample type; Copy types are converted by value.
trait IntoF32 {
    fn to_f32(self) -> f32;
}

impl IntoF32 for f32 {
    fn to_f32(self) -> f32 {
        self
    }
}
impl IntoF32 for i16 {
    fn to_f32(self) -> f32 {
        self as f32 / 32_768.0
    }
}
impl IntoF32 for u16 {
    fn to_f32(self) -> f32 {
        (self as f32 - 32_768.0) / 32_768.0
    }
}
impl IntoF32 for i8 {
    fn to_f32(self) -> f32 {
        self as f32 / 128.0
    }
}
impl IntoF32 for u8 {
    fn to_f32(self) -> f32 {
        (self as f32 - 128.0) / 128.0
    }
}

impl<S> Iterator for TeeSource<S>
where
    S: Source,
    S::Item: rodio::Sample + Copy + IntoF32,
{
    type Item = S::Item;

    fn next(&mut self) -> Option<S::Item> {
        let sample = match self.source.next() {
            Some(s) => s,
            None => {
                self.flush(); // don't strand the tail of the stream
                return None;
            }
        };
        let v = sample.to_f32();
        if self.channels <= 1 {
            self.batch.push(v);
        } else {
            self.acc += v;
            self.count += 1;
            if self.count >= self.channels {
                self.batch.push(self.acc / self.channels as f32);
                self.acc = 0.0;
                self.count = 0;
            }
        }
        if self.batch.len() >= TEE_BATCH {
            self.flush();
        }
        Some(sample)
    }
}

impl<S> Drop for TeeSource<S> {
    fn drop(&mut self) {
        self.flush();
    }
}

impl<S> Source for TeeSource<S>
where
    S: Source,
    S::Item: rodio::Sample + Copy + IntoF32,
{
    fn current_frame_len(&self) -> Option<usize> {
        self.source.current_frame_len()
    }
    fn channels(&self) -> u16 {
        self.source.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<std::time::Duration> {
        self.source.total_duration()
    }
}

pub const DEFAULT_VOLUME: f32 = 0.8;

/// Play an audio file through rodio while tapping the stream for analysis.
///
/// Sample-rate note (verified against rodio 0.19.0 source): `Sink::append`
/// feeds a `SourcesQueue` whose output the `OutputStream`'s dynamic mixer
/// (built at the *device's* default config) wraps in a `UniformSourceIterator`
/// that linearly resamples every source to the device rate and channel
/// count. Playback therefore runs at the correct pitch for any file rate,
/// and this tap sits *before* that conversion, so the analyzer correctly
/// maps FFT bins using the file's own sample rate.
pub fn start_file(path: &Path, queue: SampleQueue) -> Result<AudioHandle> {
    let file = File::open(path).with_context(|| format!("failed to open '{}'", path.display()))?;
    let decoder = Decoder::new(BufReader::new(file))
        .map_err(|e| anyhow!("failed to decode '{}' ({e})", path.display()))
        .with_context(|| "make sure the file is valid audio (mp3, wav, ogg vorbis, flac)")?;

    let sample_rate = decoder.sample_rate();
    let sample_rate = if sample_rate == 0 {
        44_100
    } else {
        sample_rate
    };
    let channels = decoder.channels().max(1);
    let description = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let tee = TeeSource::new(decoder, channels, queue);

    let (stream, stream_handle) =
        OutputStream::try_default().context("no audio output device is available")?;
    let sink = Sink::try_new(&stream_handle).context("failed to open the audio output stream")?;
    sink.set_volume(DEFAULT_VOLUME);
    sink.append(tee);

    Ok(AudioHandle {
        meta: Arc::new(Mutex::new(AudioMeta {
            sample_rate,
            description,
            status: "playing",
        })),
        sink: Some(sink),
        capture: None,
        _output: Some(stream),
        demo: false,
        demo_pause: None,
    })
}

impl IntoF32 for i32 {
    fn to_f32(self) -> f32 {
        self as f32 / 2_147_483_648.0
    }
}
impl IntoF32 for u32 {
    fn to_f32(self) -> f32 {
        (self as f32 - 2_147_483_648.0) / 2_147_483_648.0
    }
}
impl IntoF32 for i64 {
    fn to_f32(self) -> f32 {
        self as f32 / 9.223_372e18
    }
}
impl IntoF32 for u64 {
    fn to_f32(self) -> f32 {
        (self as f32 - 9.223_372e18) / 9.223_372e18
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: SampleQueue,
    channels: u16,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + IntoF32,
{
    let channels = channels.max(1);
    let mut batch: Vec<f32> = Vec::with_capacity(2048);
    let mut acc = 0.0f32;
    let mut count = 0u16;
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _info| {
            // Mix the whole callback buffer down to mono first, then hand
            // it to the queue in one bulk push (one lock per callback).
            batch.clear();
            for sample in data {
                let v = (*sample).to_f32();
                if channels <= 1 {
                    batch.push(v);
                } else {
                    acc += v;
                    count += 1;
                    if count >= channels {
                        batch.push(acc / channels as f32);
                        acc = 0.0;
                        count = 0;
                    }
                }
            }
            push_samples(&queue, &batch);
        },
        |_err| {
            // Capture glitches are non-fatal; the analyzer just sees
            // silence until the stream recovers.
        },
        None,
    )?;
    stream.play()?;
    Ok(stream)
}

/// Open one capture stream on `device`.
///
/// Config selection is direction-aware (verified against cpal 0.15.3's
/// WASAPI host): loopback opens the default *output* device as an input
/// stream, and the shared-mode mix format from `default_output_config()`
/// is the format WASAPI loopback guarantees; `default_input_config()` on
/// an output device has no formats at all. Microphones use the input
/// config. The format dispatch covers every cpal sample type.
fn open_capture_stream(
    device: &cpal::Device,
    queue: SampleQueue,
    loopback: bool,
) -> Result<(cpal::Stream, u32)> {
    let supported = if loopback {
        device.default_output_config()
    } else {
        device.default_input_config()
    }
    .context("the device reported no usable stream configuration")?;
    let sample_rate = supported.sample_rate().0;
    if sample_rate == 0 {
        bail!("device reported an invalid sample rate");
    }
    let channels = supported.channels();
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build_stream::<f32>(device, &config, queue, channels)?,
        cpal::SampleFormat::I16 => build_stream::<i16>(device, &config, queue, channels)?,
        cpal::SampleFormat::U16 => build_stream::<u16>(device, &config, queue, channels)?,
        cpal::SampleFormat::I8 => build_stream::<i8>(device, &config, queue, channels)?,
        cpal::SampleFormat::U8 => build_stream::<u8>(device, &config, queue, channels)?,
        cpal::SampleFormat::I32 => build_stream::<i32>(device, &config, queue, channels)?,
        cpal::SampleFormat::U32 => build_stream::<u32>(device, &config, queue, channels)?,
        cpal::SampleFormat::I64 => build_stream::<i64>(device, &config, queue, channels)?,
        cpal::SampleFormat::U64 => build_stream::<u64>(device, &config, queue, channels)?,
        other => bail!("unsupported capture sample format: {other:?}"),
    };
    Ok((stream, sample_rate))
}

fn describe(source: CaptureSource, device_name: &str, status: &str) -> String {
    format!(
        "{source} \u{b7} {device_name} \u{b7} {status}",
        source = source.name()
    )
}

/// Capture live audio from an explicitly chosen source. No silent
/// fallback: system capture failing is an error that tells the user what
/// to try next.
///
/// All device and stream handling happens on a dedicated capture-owner
/// thread, because `cpal::Stream` is `!Send` on every platform. For system
/// capture on Windows the owner re-resolves the default *output* device
/// every second and transparently rebuilds the WASAPI loopback stream
/// when it changes, carrying the pause state over.
pub fn start_capture(queue: SampleQueue, source: CaptureSource) -> Result<AudioHandle> {
    let (tx, cmd_rx) = mpsc::channel::<CapCmd>();
    let capture = Arc::new(Mutex::new(CaptureShared {
        tx,
        device_name: String::new(),
        paused: false,
        last_error: None,
    }));
    let meta = Arc::new(Mutex::new(AudioMeta {
        sample_rate: 0,
        description: String::new(),
        status: "starting",
    }));
    let (init_tx, init_rx) = mpsc::channel::<Result<(String, u32), String>>();

    std::thread::Builder::new()
        .name("spectrum-capture".into())
        .spawn({
            let capture = capture.clone();
            let meta = meta.clone();
            move || capture_owner_loop(source, queue, cmd_rx, capture, meta, init_tx)
        })
        .context("failed to spawn the capture thread")?;

    // Wait for the initial build on the owner thread so failures surface
    // here as honest, actionable errors.
    let init = init_rx
        .recv()
        .map_err(|_| anyhow!("the capture thread exited before opening a stream"))?;
    let (device_name, sample_rate) = init.map_err(|err| {
        let advice = match source {
            CaptureSource::System => {
                "system-output capture failed: make sure an audio output device is \
                 enabled, or run with --source mic to use a microphone instead"
            }
            CaptureSource::Microphone => {
                "microphone capture failed: check that a recording device is \
                 enabled and not in use exclusively, or run with --source system"
            }
        };
        anyhow!("{advice}\ncaused by: {err}")
    })?;

    {
        let mut m = meta.lock().unwrap_or_else(|e| e.into_inner());
        m.sample_rate = sample_rate;
        m.description = describe(source, &device_name, "live");
        m.status = "live";
    }

    Ok(AudioHandle {
        meta,
        sink: None,
        capture: Some(capture),
        _output: None,
        demo: false,
        demo_pause: None,
    })
}

/// The capture owner thread body: resolves the initial device, builds the
/// stream, then serves pause/play commands and re-checks the default
/// output device about once a second (system capture only), rebuilding the
/// loopback stream on change. Exits when the command channel closes (the
/// `AudioHandle` was dropped), dropping the stream and stopping capture.
fn capture_owner_loop(
    source: CaptureSource,
    queue: SampleQueue,
    cmd_rx: Receiver<CapCmd>,
    shared: Arc<Mutex<CaptureShared>>,
    meta: Arc<Mutex<AudioMeta>>,
    init_tx: Sender<Result<(String, u32), String>>,
) {
    use cpal::traits::DeviceTrait;

    let loopback = source == CaptureSource::System && cfg!(windows);

    // Resolve + build the first stream on this thread.
    let Some(device) = resolve_device(source) else {
        let _ = init_tx.send(Err(format!(
            "no {} device found",
            match source {
                CaptureSource::System => "default output",
                CaptureSource::Microphone => "default input",
            }
        )));
        return;
    };
    let mut device_name = device
        .name()
        .map(|n| {
            if n.trim().is_empty() {
                "default device".into()
            } else {
                n
            }
        })
        .unwrap_or_else(|_| "default device".into());
    let mut stream = match open_capture_stream(&device, queue.clone(), loopback) {
        Ok((s, rate)) => {
            {
                let mut sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                sh.device_name = device_name.clone();
            }
            let _ = init_tx.send(Ok((device_name.clone(), rate)));
            s
        }
        Err(err) => {
            let _ = init_tx.send(Err(format!(
                "failed to open {} on '{device_name}': {err:#}",
                source.name()
            )));
            return;
        }
    };

    let mut last_check = std::time::Instant::now();
    loop {
        match cmd_rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(CapCmd::Pause) => {
                let _ = stream.pause();
            }
            Ok(CapCmd::Play) => {
                let _ = stream.play();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        // Re-check the default output device about once a second.
        if source != CaptureSource::System
            || last_check.elapsed() < std::time::Duration::from_secs(1)
        {
            continue;
        }
        last_check = std::time::Instant::now();

        let Some(new_device) = resolve_device(source) else {
            continue;
        };
        let Ok(new_name) = new_device.name() else {
            continue;
        };
        if new_name == device_name {
            continue;
        }
        // The default output changed: rebuild the loopback stream on it,
        // carrying the pause state over.
        let paused = shared.lock().unwrap_or_else(|e| e.into_inner()).paused;
        match open_capture_stream(&new_device, queue.clone(), true) {
            Ok((new_stream, rate)) => {
                if paused {
                    let _ = new_stream.pause();
                }
                stream = new_stream;
                device_name = new_name;
                let mut sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                sh.device_name = device_name.clone();
                sh.last_error = None;
                let mut m = meta.lock().unwrap_or_else(|e| e.into_inner());
                m.sample_rate = rate;
                m.description = describe(source, &device_name, "live");
                m.status = "live";
            }
            Err(err) => {
                let detail = format!("{err:#}");
                let mut sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                sh.last_error = Some(detail.clone());
                let mut m = meta.lock().unwrap_or_else(|e| e.into_inner());
                m.description = format!("{} \u{b7} reconnecting ({detail})", source.name());
                m.status = "reconnecting";
            }
        }
    }
    // Channel closed: dropping `stream` stops capture.
    drop(stream);
}

fn resolve_device(source: CaptureSource) -> Option<cpal::Device> {
    use cpal::traits::HostTrait;
    let host = cpal::default_host();
    match source {
        CaptureSource::System => host.default_output_device(),
        CaptureSource::Microphone => host.default_input_device(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal f32 source yielding 1.0, 2.0, 3.0, ...
    struct RampSource {
        len: usize,
        pos: usize,
        channels: u16,
        rate: u32,
    }

    impl Iterator for RampSource {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            if self.pos >= self.len {
                return None;
            }
            self.pos += 1;
            Some(self.pos as f32)
        }
    }

    impl Source for RampSource {
        fn current_frame_len(&self) -> Option<usize> {
            Some(self.len - self.pos)
        }
        fn channels(&self) -> u16 {
            self.channels
        }
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn total_duration(&self) -> Option<std::time::Duration> {
            None
        }
    }

    fn drain_queue(queue: &SampleQueue) -> Vec<f32> {
        queue.lock().unwrap().drain(..).collect()
    }

    #[test]
    fn tee_mixes_stereo_and_flushes_tail() {
        let queue = new_queue();
        // 6 samples = 3 stereo frames: (1,2) (3,4) (5,6) -> mono 1.5, 3.5, 5.5
        let src = RampSource {
            len: 6,
            pos: 0,
            channels: 2,
            rate: 44_100,
        };
        let mut tee = TeeSource::new(src, 2, queue.clone());

        let mut out = 0;
        while tee.next().is_some() {
            out += 1;
        }
        assert_eq!(out, 6);

        let drained = drain_queue(&queue);
        let expect = [1.5, 3.5, 5.5];
        assert_eq!(drained.len(), expect.len(), "tail must flush on end");
        for (a, b) in drained.iter().zip(expect.iter()) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn tee_batches_midstream_without_loss() {
        let queue = new_queue();
        // 1500 mono samples: forces a flush at TEE_BATCH (512) mid-stream
        let src = RampSource {
            len: 1500,
            pos: 0,
            channels: 1,
            rate: 44_100,
        };
        let mut tee = TeeSource::new(src, 1, queue.clone());
        for _ in 0..1000 {
            tee.next();
        }
        // Exactly one full batch should have been flushed so far.
        let partial = drain_queue(&queue);
        assert_eq!(partial.len(), TEE_BATCH);

        while tee.next().is_some() {}
        let rest = drain_queue(&queue);
        assert_eq!(partial.len() + rest.len(), 1500, "no sample may be lost");
    }

    #[test]
    fn queue_overflow_drops_oldest() {
        let queue = new_queue();
        let big: Vec<f32> = (0..QUEUE_CAP + 100).map(|i| i as f32).collect();
        push_samples(&queue, &big);
        let drained = drain_queue(&queue);
        assert_eq!(drained.len(), QUEUE_CAP);
        // Oldest samples dropped, newest retained in order.
        assert_eq!(drained[0], 100.0);
        assert_eq!(drained[QUEUE_CAP - 1], (QUEUE_CAP + 99) as f32);
    }

    #[test]
    fn non_finite_samples_never_enter_queue() {
        let queue = new_queue();
        push_samples(&queue, &[f32::NAN, f32::INFINITY, 0.5]);
        assert_eq!(drain_queue(&queue), vec![0.5]);
    }
}
