//! Canlı çözücü: SDR'dan gelen IQ bloklarını DVB-S2 alıcısından geçirir ve
//! çıkan MPEG-TS'yi video oynatıcıya (ffplay) aktarır.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::thread;

use num_complex::Complex;
use qoda::dvbs2::receiver::{Receiver as DvbReceiver, ReceiverConfig, ReceiverStats};

/// SDR ile çözücü arasında bekleyebilecek en fazla IQ bloğu.
const QUEUE_BLOCKS: usize = 64;

#[derive(Default, Clone)]
pub struct DecoderStatus {
    pub stats: ReceiverStats,
    /// Oynatıcı sorunu ya da kapanması gibi bilgiler.
    pub player: Option<String>,
}

/// Çalışan çözücüye bağlantı. Düşürüldüğünde çözücü ve oynatıcı kapanır.
pub struct Decoder {
    pub iq: SyncSender<Vec<Complex<f32>>>,
    pub status: Arc<Mutex<DecoderStatus>>,
    /// SDR tarafında, çözücü yetişemediği için atlanan IQ blokları.
    pub dropped: Arc<AtomicU64>,
}

pub fn spawn(cfg: ReceiverConfig, title: String) -> Decoder {
    let (tx, rx) = sync_channel(QUEUE_BLOCKS);
    let status = Arc::new(Mutex::new(DecoderStatus::default()));
    let shared = Arc::clone(&status);
    thread::spawn(move || run(cfg, title, rx, shared));
    Decoder { iq: tx, status, dropped: Arc::new(AtomicU64::new(0)) }
}

fn run(cfg: ReceiverConfig, title: String, rx: Receiver<Vec<Complex<f32>>>, status: Arc<Mutex<DecoderStatus>>) {
    let mut receiver = DvbReceiver::new(cfg);
    let mut player = match start_player(&title) {
        Ok(child) => Some(child),
        Err(e) => {
            status.lock().unwrap().player = Some(e);
            None
        }
    };
    let mut ts = Vec::new();
    while let Ok(block) = rx.recv() {
        ts.clear();
        receiver.process(&block, &mut ts);
        if !ts.is_empty() {
            if let Some(child) = &mut player {
                let ok = child.stdin.as_mut().is_some_and(|stdin| stdin.write_all(&ts).is_ok());
                if !ok {
                    status.lock().unwrap().player = Some("Video penceresi kapandı".into());
                    let _ = child.kill();
                    player = None;
                }
            }
        }
        status.lock().unwrap().stats = receiver.stats.clone();
    }
    if let Some(mut child) = player {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// ffplay'i standart girdiden TS okuyacak şekilde başlatır.
fn start_player(title: &str) -> Result<Child, String> {
    let exe = find_ffplay().ok_or("ffplay bulunamadı (FFmpeg kurulu ve PATH'te olmalı)")?;
    Command::new(exe)
        .args(["-hide_banner", "-loglevel", "error", "-fflags", "nobuffer", "-framedrop"])
        .args(["-window_title", title, "-i", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("ffplay başlatılamadı: {e}"))
}

/// Önce PATH'te, Windows'ta ayrıca winget'in FFmpeg kurulum klasöründe arar.
fn find_ffplay() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ffplay.exe" } else { "ffplay" };
    let in_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
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
        .map(|build| build.path().join("bin").join(name))
        .find(|p| p.is_file())
}
