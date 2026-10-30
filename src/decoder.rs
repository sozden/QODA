//! Live decoder: passes IQ blocks from the SDR through the DVB-S2 receiver and sends
//! the resulting MPEG-TS to ffmpeg for video and to a windowless ffplay for audio.
//!
//! ffmpeg decodes the TS into fixed-size raw RGBA frames, which the UI shows as a
//! texture. The decoder never waits for a player: writes happen on separate
//! threads, and when a queue is full a TS chunk is dropped, never IQ.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use num_complex::Complex;
use qoda::dvbs2::receiver::{Receiver as DvbReceiver, ReceiverConfig, ReceiverStats};

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

/// If `audio` is false no audio player is started. `notify` is called on every new frame.
pub fn spawn(cfg: ReceiverConfig, audio: bool, notify: impl Fn() + Send + 'static) -> Decoder {
    let (tx, rx) = sync_channel(QUEUE_BLOCKS);
    let status = Arc::new(Mutex::new(DecoderStatus::default()));
    let video = Arc::new(Mutex::new(None));
    let (s, v) = (Arc::clone(&status), Arc::clone(&video));
    thread::spawn(move || run(cfg, audio, rx, s, v, notify));
    Decoder { iq: tx, status, dropped: Arc::new(AtomicU64::new(0)), video }
}

fn run(
    cfg: ReceiverConfig,
    audio: bool,
    rx: Receiver<Vec<Complex<f32>>>,
    status: Arc<Mutex<DecoderStatus>>,
    video: Arc<Mutex<Option<VideoFrame>>>,
    notify: impl Fn() + Send + 'static,
) {
    let mut receiver = DvbReceiver::new(cfg);
    let mut sinks: Vec<SyncSender<Arc<Vec<u8>>>> = Vec::new();
    let mut children: Vec<Child> = Vec::new();

    match start_video(Arc::clone(&video), notify) {
        Ok((child, stdin)) => {
            sinks.push(spawn_writer(stdin, Arc::clone(&status)));
            children.push(child);
        }
        Err(e) => status.lock().unwrap().player = Some(e),
    }
    if audio {
        match start_audio() {
            Ok(mut child) => {
                let stdin = child.stdin.take().unwrap();
                sinks.push(spawn_writer(stdin, Arc::clone(&status)));
                children.push(child);
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

/// Plays audio with a windowless ffplay.
fn start_audio() -> Result<Child, PlayerIssue> {
    let exe = find_ffmpeg_tool("ffplay").ok_or(PlayerIssue::FfplayMissing)?;
    background(Command::new(exe))
        .args(["-hide_banner", "-loglevel", "error", "-nodisp", "-infbuf", "-vn", "-sn", "-i", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| PlayerIssue::StartFailed { program: "ffplay", error: e.to_string() })
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

/// Looks for an FFmpeg tool next to the program first (packaged build), then in PATH,
/// and on Windows also in winget's install folder.
pub fn find_ffmpeg_tool(tool: &str) -> Option<PathBuf> {
    let name = if cfg!(windows) { format!("{tool}.exe") } else { tool.to_string() };
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let bundled = [dir.join(&name), dir.join("ffmpeg").join(&name)].into_iter().find(|p| p.is_file());
        if bundled.is_some() {
            return bundled;
        }
    }
    let in_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join(&name))
        .find(|p| p.is_file());
    if in_path.is_some() {
        return in_path;
    }
    let packages = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Microsoft/WinGet/Packages");
    std::fs::read_dir(packages)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("FFmpeg"))
        .flat_map(|pkg| std::fs::read_dir(pkg.path()).into_iter().flatten().flatten())
        .map(|build| build.path().join("bin").join(&name))
        .find(|p| p.is_file())
}
