//! Finding the FFmpeg programs, which the receiver uses for playback and the transmitter for
//! making test broadcasts.

use std::path::PathBuf;

/// Looks for an FFmpeg tool next to the program first (packaged build), then in PATH,
/// and on Windows also in winget's install folder.
pub fn find_tool(tool: &str) -> Option<PathBuf> {
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
