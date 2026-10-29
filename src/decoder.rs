//! Canlı çözücü: SDR'dan gelen IQ bloklarını DVB-S2 alıcısından geçirir; çıkan
//! MPEG-TS'yi görüntü için ffmpeg'e, ses için penceresiz ffplay'e aktarır.
//!
//! ffmpeg TS'yi çözüp sabit boyutlu ham RGBA kareler üretir; kareler arayüzde
//! doku olarak gösterilir. Çözücü hiçbir oynatıcıyı beklemez: yazma işleri ayrı
//! iş parçacıklarındadır ve kuyruk dolarsa IQ değil TS parçası atlanır.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use num_complex::Complex;
use qoda::dvbs2::receiver::{Receiver as DvbReceiver, ReceiverConfig, ReceiverStats};

/// SDR ile çözücü arasında bekleyebilecek en fazla IQ bloğu.
const QUEUE_BLOCKS: usize = 64;
/// Çözücü ile her oynatıcı arasında bekleyebilecek en fazla TS parçası.
const TS_QUEUE_CHUNKS: usize = 1024;
/// ffmpeg'in ürettiği kare boyutu; görüntü en-boy oranı korunarak sığdırılır.
pub const VIDEO_WIDTH: usize = 1280;
pub const VIDEO_HEIGHT: usize = 720;

#[derive(Default, Clone)]
pub struct DecoderStatus {
    pub stats: ReceiverStats,
    /// Oynatıcı sorunları gibi bilgiler.
    pub player: Option<String>,
    /// Oynatıcılar yetişemediği için atlanan TS parçaları.
    pub ts_dropped: u64,
}

/// Son çözülen video karesi.
pub struct VideoFrame {
    pub rgba: Vec<u8>,
    /// Her yeni karede artar; arayüz yalnızca değişince dokuyu günceller.
    pub seq: u64,
}

/// Çalışan çözücüye bağlantı. Düşürüldüğünde çözücü ve oynatıcılar kapanır.
pub struct Decoder {
    pub iq: SyncSender<Vec<Complex<f32>>>,
    pub status: Arc<Mutex<DecoderStatus>>,
    /// SDR tarafında, çözücü yetişemediği için atlanan IQ blokları.
    pub dropped: Arc<AtomicU64>,
    pub video: Arc<Mutex<Option<VideoFrame>>>,
}

/// `audio` false ise ses oynatıcısı başlatılmaz. `notify` her yeni karede çağrılır.
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

/// TS parçalarını bir oynatıcının standart girdisine yazan iş parçacığı.
fn spawn_writer(mut stdin: ChildStdin, status: Arc<Mutex<DecoderStatus>>) -> SyncSender<Arc<Vec<u8>>> {
    let (tx, rx) = sync_channel::<Arc<Vec<u8>>>(TS_QUEUE_CHUNKS);
    thread::spawn(move || {
        while let Ok(chunk) = rx.recv() {
            if stdin.write_all(&chunk).is_err() {
                status.lock().unwrap().player = Some("Oynatıcı kapandı".into());
                break;
            }
        }
        // Gönderen taraf takılmasın diye kuyruğu boşaltmaya devam et.
        while rx.recv().is_ok() {}
    });
    tx
}

/// ffmpeg'i TS'den ham RGBA kare üretecek şekilde başlatır ve kareleri okuyan
/// iş parçacığını kurar.
fn start_video(
    video: Arc<Mutex<Option<VideoFrame>>>,
    notify: impl Fn() + Send + 'static,
) -> Result<(Child, ChildStdin), String> {
    let exe = find_ffmpeg_tool("ffmpeg").ok_or("ffmpeg bulunamadı (FFmpeg kurulu ve PATH'te olmalı)")?;
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
        .map_err(|e| format!("ffmpeg başlatılamadı: {e}"))?;
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

/// Sesi penceresiz ffplay ile çalar.
fn start_audio() -> Result<Child, String> {
    let exe = find_ffmpeg_tool("ffplay").ok_or("ses için ffplay bulunamadı")?;
    background(Command::new(exe))
        .args(["-hide_banner", "-loglevel", "error", "-nodisp", "-infbuf", "-vn", "-sn", "-i", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("ffplay başlatılamadı: {e}"))
}

/// Windows'ta alt sürecin kendi konsol penceresini açmasını engeller.
fn background(mut cmd: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// FFmpeg aracını önce PATH'te, Windows'ta ayrıca winget'in kurulum klasöründe arar.
fn find_ffmpeg_tool(tool: &str) -> Option<PathBuf> {
    let name = if cfg!(windows) { format!("{tool}.exe") } else { tool.to_string() };
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
