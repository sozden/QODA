//! Canlı çözücü: SDR'dan gelen IQ bloklarını DVB-S2 alıcısından geçirir ve
//! çıkan MPEG-TS'yi video oynatıcıya (ffplay) aktarır.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::thread;

use num_complex::Complex;
use qoda::dvbs2::receiver::{Receiver as DvbReceiver, ReceiverConfig, ReceiverStats};

/// SDR ile çözücü arasında bekleyebilecek en fazla IQ bloğu.
const QUEUE_BLOCKS: usize = 64;
/// Çözücü ile oynatıcı arasında bekleyebilecek en fazla TS parçası.
const TS_QUEUE_CHUNKS: usize = 1024;

#[derive(Default, Clone)]
pub struct DecoderStatus {
    pub stats: ReceiverStats,
    /// Oynatıcı sorunu ya da kapanması gibi bilgiler.
    pub player: Option<String>,
    /// Oynatıcı yetişemediği için atlanan TS parçaları.
    pub ts_dropped: u64,
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
    // TS ayrı bir iş parçacığıyla oynatıcıya yazılır: oynatıcı okumayı geciktirse de
    // çözücü beklemez. Kuyruk dolarsa IQ değil TS atlanır.
    let (ts_tx, ts_rx) = sync_channel::<Vec<u8>>(TS_QUEUE_CHUNKS);
    let writer_status = Arc::clone(&status);
    let writer = thread::spawn(move || write_to_player(&title, ts_rx, writer_status));

    let mut ts = Vec::new();
    let mut ts_dropped = 0u64;
    while let Ok(block) = rx.recv() {
        ts.clear();
        receiver.process(&block, &mut ts);
        if !ts.is_empty() {
            match ts_tx.try_send(ts.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => ts_dropped += 1,
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
        let mut st = status.lock().unwrap();
        st.stats = receiver.stats.clone();
        st.ts_dropped = ts_dropped;
    }
    drop(ts_tx);
    let _ = writer.join();
}

fn write_to_player(title: &str, ts: Receiver<Vec<u8>>, status: Arc<Mutex<DecoderStatus>>) {
    let mut child = match start_player(title) {
        Ok(child) => child,
        Err(e) => {
            status.lock().unwrap().player = Some(e);
            // Kuyruğu boşaltmaya devam et ki çözücü TS atladığını sanmasın.
            while ts.recv().is_ok() {}
            return;
        }
    };
    while let Ok(chunk) = ts.recv() {
        let ok = child.stdin.as_mut().is_some_and(|stdin| stdin.write_all(&chunk).is_ok());
        if !ok {
            status.lock().unwrap().player = Some("Video penceresi kapandı".into());
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    while ts.recv().is_ok() {}
}

/// ffplay'i standart girdiden TS okuyacak şekilde başlatır.
fn start_player(title: &str) -> Result<Child, String> {
    let exe = find_ffplay().ok_or("ffplay bulunamadı (FFmpeg kurulu ve PATH'te olmalı)")?;
    Command::new(exe)
        // -infbuf: ffplay giriş tamponunu sınırlamaz, okumayı hiç durdurmaz.
        .args(["-hide_banner", "-loglevel", "error", "-infbuf", "-framedrop"])
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
