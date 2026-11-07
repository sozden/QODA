//! Live decoder: passes IQ blocks from the SDR through the DVB-S2 receiver and sends
//! the resulting MPEG-TS to ffmpeg for video and for audio; the audio is scaled here
//! and played by a windowless ffplay.
//!
//! ffmpeg decodes the TS into fixed-size raw RGBA frames, which the UI shows as a
//! texture. The decoder never waits for a player: writes happen on separate
//! threads, and when a queue is full a TS chunk is dropped, never IQ.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use num_complex::Complex;
use qoda::dvbs2::receiver::{Receiver as DvbReceiver, ReceiverConfig, ReceiverStats};

use qoda::monitor::{Monitor, MonitorConfig};

/// Maximum number of IQ blocks that may wait between the SDR and the decoder.
const QUEUE_BLOCKS: usize = 64;
/// Maximum number of TS chunks that may wait between the decoder and each player.
const TS_QUEUE_CHUNKS: usize = 1024;
/// Frame size produced by ffmpeg; the picture is fitted keeping its aspect ratio.
pub const VIDEO_WIDTH: usize = 1280;
pub const VIDEO_HEIGHT: usize = 720;

/// Player problems; the UI turns them into text in its own language.
#[derive(Debug, Clone)]
pub enum PlayerIssue {
    FfmpegMissing,
    FfplayMissing,
    StartFailed { program: &'static str, error: String },
    Closed,
}

#[derive(Default, Clone)]
pub struct DecoderStatus {
    pub stats: ReceiverStats,
    pub player: Option<PlayerIssue>,
    /// TS chunks dropped because the players could not keep up.
    pub ts_dropped: u64,
}

/// Last decoded video frame.
pub struct VideoFrame {
    pub rgba: Vec<u8>,
    /// Increases with every new frame; the UI updates the texture only when it changes.
    pub seq: u64,
}

/// Handle to the running decoder. Dropping it shuts down the decoder and the players.
pub struct Decoder {
    pub iq: SyncSender<Vec<Complex<f32>>>,
    pub status: Arc<Mutex<DecoderStatus>>,
    /// IQ blocks dropped on the SDR side because the decoder could not keep up.
    pub dropped: Arc<AtomicU64>,
    pub video: Arc<Mutex<Option<VideoFrame>>>,
}

/// If `audio` is None no audio player is started; if `monitor` is None nothing is sent over UDP.
/// `notify` is called on every new frame.
pub fn spawn(
    cfg: ReceiverConfig,
    audio: Option<Arc<AudioControl>>,
    monitor: Option<MonitorConfig>,
    notify: impl Fn() + Send + 'static,
) -> Decoder {
    let (tx, rx) = sync_channel(QUEUE_BLOCKS);
    let status = Arc::new(Mutex::new(DecoderStatus::default()));
    let video = Arc::new(Mutex::new(None));
    let (s, v) = (Arc::clone(&status), Arc::clone(&video));
    thread::spawn(move || run(cfg, audio, monitor, rx, s, v, notify));
    Decoder { iq: tx, status, dropped: Arc::new(AtomicU64::new(0)), video }
}

fn run(
    cfg: ReceiverConfig,
    audio: Option<Arc<AudioControl>>,
    monitor: Option<MonitorConfig>,
    rx: Receiver<Vec<Complex<f32>>>,
    status: Arc<Mutex<DecoderStatus>>,
    video: Arc<Mutex<Option<VideoFrame>>>,
    notify: impl Fn() + Send + 'static,
) {
    let mut receiver = DvbReceiver::new(cfg);
    let mut monitor = monitor.and_then(|m| Monitor::new(m).ok());
    let mut sinks: Vec<SyncSender<Arc<Vec<u8>>>> = Vec::new();
    let mut children: Vec<Child> = Vec::new();

    match start_video(Arc::clone(&video), notify) {
        Ok((child, stdin)) => {
            sinks.push(spawn_writer(stdin, Arc::clone(&status)));
            children.push(child);
        }
        Err(e) => status.lock().unwrap().player = Some(e),
    }
    if let Some(control) = audio {
        match start_audio(control) {
            Ok((procs, stdin)) => {
                sinks.push(spawn_writer(stdin, Arc::clone(&status)));
                children.extend(procs);
            }
            Err(e) => status.lock().unwrap().player = Some(e),
        }
    }

    let mut ts = Vec::new();
    let mut ts_dropped = 0u64;
    while let Ok(block) = rx.recv() {
        ts.clear();
        receiver.process(&block, &mut ts);
        if !ts.is_empty() {
            let chunk = Arc::new(ts.clone());
            for sink in &sinks {
                if let Err(TrySendError::Full(_)) = sink.try_send(Arc::clone(&chunk)) {
                    ts_dropped += 1;
                }
            }
            if let Some(m) = &mut monitor {
                m.send_ts(&ts);
            }
        }
        if let Some(m) = &mut monitor {
            m.maybe_send_telemetry(&receiver.stats);
        }
        let mut st = status.lock().unwrap();
        st.stats = receiver.stats.clone();
        st.ts_dropped = ts_dropped;
    }
    drop(sinks);
    for mut child in children {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Thread that writes TS chunks to a player's standard input.
fn spawn_writer(mut stdin: ChildStdin, status: Arc<Mutex<DecoderStatus>>) -> SyncSender<Arc<Vec<u8>>> {
    let (tx, rx) = sync_channel::<Arc<Vec<u8>>>(TS_QUEUE_CHUNKS);
    thread::spawn(move || {
        while let Ok(chunk) = rx.recv() {
            if stdin.write_all(&chunk).is_err() {
                status.lock().unwrap().player = Some(PlayerIssue::Closed);
                break;
            }
        }
        // Keep draining the queue so the sender does not block.
        while rx.recv().is_ok() {}
    });
    tx
}

/// Starts ffmpeg to produce raw RGBA frames from the TS and sets up the thread
/// that reads the frames.
fn start_video(
    video: Arc<Mutex<Option<VideoFrame>>>,
    notify: impl Fn() + Send + 'static,
) -> Result<(Child, ChildStdin), PlayerIssue> {
    let exe = find_ffmpeg_tool("ffmpeg").ok_or(PlayerIssue::FfmpegMissing)?;
    let filter = format!(
        "scale={VIDEO_WIDTH}:{VIDEO_HEIGHT}:force_original_aspect_ratio=decrease,\
         pad={VIDEO_WIDTH}:{VIDEO_HEIGHT}:(ow-iw)/2:(oh-ih)/2"
    );
    let mut child = background(Command::new(exe))
        .args(["-hide_banner", "-loglevel", "error", "-fflags", "nobuffer", "-flags", "low_delay"])
        .args(["-probesize", "500000", "-analyzeduration", "1000000", "-i", "-"])
        .args(["-an", "-sn", "-vf", &filter, "-pix_fmt", "rgba", "-f", "rawvideo", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| PlayerIssue::StartFailed { program: "ffmpeg", error: e.to_string() })?;
    let stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    thread::spawn(move || {
        let mut seq = 0;
        loop {
            let mut rgba = vec![0u8; VIDEO_WIDTH * VIDEO_HEIGHT * 4];
            if stdout.read_exact(&mut rgba).is_err() {
                return;
            }
            seq += 1;
            *video.lock().unwrap() = Some(VideoFrame { rgba, seq });
            notify();
        }
    });
    Ok((child, stdin))
}

/// Volume settings shared between the UI and the audio thread; changes apply at once.
pub struct AudioControl {
    /// Linear gain stored as f32 bits.
    volume: AtomicU32,
    /// Keeps the loudness near `AUTO_TARGET`.
    auto_level: AtomicBool,
}

impl AudioControl {
    pub fn new(volume: f32, auto_level: bool) -> Self {
        Self { volume: AtomicU32::new(volume.to_bits()), auto_level: AtomicBool::new(auto_level) }
    }
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn auto_level(&self) -> bool {
        self.auto_level.load(Ordering::Relaxed)
    }
    pub fn set_auto_level(&self, on: bool) {
        self.auto_level.store(on, Ordering::Relaxed);
    }
}

const AUDIO_RATE: f32 = 48_000.0;
/// RMS level the automatic level aims for (about -20 dBFS).
const AUTO_TARGET: f32 = 0.1;
/// Limits of the automatic gain.
const AUTO_MIN_GAIN: f32 = 0.1;
const AUTO_MAX_GAIN: f32 = 10.0;
/// Block RMS below which the audio is taken as a pause and the level is held, so
/// pauses and noise between programmes are not brought up (-46 dBFS).
const SILENCE: f32 = 0.005;
/// Audio frames per level measurement (about 21 ms).
const LEVEL_BLOCK: usize = 1024;

/// Automatic level: measures the RMS level per block and follows it quickly when it
/// rises and slowly when it falls, so loud passages are pulled down at once and quiet
/// ones brought up gently. The gain glides sample by sample to avoid clicks.
struct AutoLevel {
    power: f32,
    gain: f32,
    wanted: f32,
    block_sum: f32,
    block_len: usize,
}

impl AutoLevel {
    fn new() -> Self {
        Self { power: AUTO_TARGET * AUTO_TARGET, gain: 1.0, wanted: 1.0, block_sum: 0.0, block_len: 0 }
    }

    /// Gain for the next frame, given its mean square.
    fn next(&mut self, square: f32) -> f32 {
        self.block_sum += square;
        self.block_len += 1;
        if self.block_len == LEVEL_BLOCK {
            let block = self.block_sum / LEVEL_BLOCK as f32;
            self.block_sum = 0.0;
            self.block_len = 0;
            if block.sqrt() > SILENCE {
                // Time constants: 0.1 s rising, 3 s falling.
                let seconds = if block > self.power { 0.1 } else { 3.0 };
                let a = LEVEL_BLOCK as f32 / (seconds * AUDIO_RATE);
                self.power += (block - self.power) * a.min(1.0);
                self.wanted = (AUTO_TARGET / self.power.sqrt()).clamp(AUTO_MIN_GAIN, AUTO_MAX_GAIN);
            }
        }
        self.gain += (self.wanted - self.gain) * (1.0 / (0.05 * AUDIO_RATE));
        self.gain
    }
}

/// Decodes the audio with ffmpeg to 48 kHz stereo PCM, applies the volume and the
/// automatic level, and plays it with a windowless ffplay.
fn start_audio(control: Arc<AudioControl>) -> Result<(Vec<Child>, ChildStdin), PlayerIssue> {
    let ffmpeg = find_ffmpeg_tool("ffmpeg").ok_or(PlayerIssue::FfmpegMissing)?;
    let ffplay = find_ffmpeg_tool("ffplay").ok_or(PlayerIssue::FfplayMissing)?;
    let mut decoder = background(Command::new(ffmpeg))
        .args(["-hide_banner", "-loglevel", "error", "-fflags", "nobuffer"])
        .args(["-probesize", "500000", "-analyzeduration", "1000000", "-i", "-"])
        .args(["-vn", "-sn", "-ac", "2", "-ar", "48000", "-f", "s16le", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| PlayerIssue::StartFailed { program: "ffmpeg", error: e.to_string() })?;
    let mut player = match background(Command::new(ffplay))
        .args(["-hide_banner", "-loglevel", "error", "-nodisp", "-infbuf"])
        .args(["-f", "s16le", "-ch_layout", "stereo", "-sample_rate", "48000", "-i", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(p) => p,
        Err(e) => {
            let _ = decoder.kill();
            return Err(PlayerIssue::StartFailed { program: "ffplay", error: e.to_string() });
        }
    };
    let stdin = decoder.stdin.take().unwrap();
    let mut pcm_in = decoder.stdout.take().unwrap();
    let mut pcm_out = player.stdin.take().unwrap();
    thread::spawn(move || {
        let mut auto = AutoLevel::new();
        let mut buf = vec![0u8; 4096];
        loop {
            if pcm_in.read_exact(&mut buf).is_err() {
                return;
            }
            let volume = control.volume();
            let auto_on = control.auto_level();
            // Interleaved stereo: one gain per frame of two samples.
            for frame in buf.chunks_exact_mut(4) {
                let l = i16::from_le_bytes([frame[0], frame[1]]) as f32 / 32768.0;
                let r = i16::from_le_bytes([frame[2], frame[3]]) as f32 / 32768.0;
                let mut gain = volume;
                if auto_on {
                    gain *= auto.next(0.5 * (l * l + r * r));
                }
                for (i, s) in [l, r].into_iter().enumerate() {
                    let v = ((s * gain).clamp(-1.0, 1.0) * 32767.0) as i16;
                    frame[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
                }
            }
            if pcm_out.write_all(&buf).is_err() {
                return;
            }
        }
    });
    Ok((vec![decoder, player], stdin))
}

/// Stops the child process from opening its own console window on Windows.
#[cfg(windows)]
fn background(mut cmd: Command) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(not(windows))]
fn background(cmd: Command) -> Command {
    cmd
}

pub use qoda::ffmpeg::find_tool as find_ffmpeg_tool;

#[cfg(test)]
mod tests {
    use super::*;

    /// Output RMS after `seconds` of a sine wave of the given amplitude.
    fn settled_rms(amplitude: f32, seconds: f32) -> f32 {
        let mut auto = AutoLevel::new();
        let n = (seconds * AUDIO_RATE) as usize;
        let mut sum = 0.0;
        let tail = AUDIO_RATE as usize / 10;
        for i in 0..n {
            let s = amplitude * (i as f32 * 0.05).sin();
            let out = s * auto.next(s * s);
            if i >= n - tail {
                sum += out * out;
            }
        }
        (sum / tail as f32).sqrt()
    }

    #[test]
    fn automatic_level_brings_loud_and_quiet_audio_together() {
        let loud = settled_rms(0.9, 3.0);
        let quiet = settled_rms(0.03, 20.0);
        assert!((loud / AUTO_TARGET - 1.0).abs() < 0.2, "loud {loud}");
        assert!((quiet / AUTO_TARGET - 1.0).abs() < 0.2, "quiet {quiet}");
    }

    #[test]
    fn automatic_level_holds_its_gain_through_silence() {
        let mut auto = AutoLevel::new();
        for _ in 0..AUDIO_RATE as usize {
            auto.next(0.0);
        }
        assert!(auto.gain <= 1.0 + 1e-6);
    }
}
