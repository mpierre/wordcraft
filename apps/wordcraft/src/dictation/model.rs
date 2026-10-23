//! The speech model: Whisper "base" (multilingual, about 148 MB, MIT licence, OpenAI), in
//! whisper.cpp's format. It isn't shipped with the app: the first Dictate fetches it from the
//! whisper.cpp model repository on Hugging Face, at a pinned revision, and checks its SHA-256
//! before keeping it in `Models` in the settings folder. Nothing else is ever sent or fetched;
//! the speech itself never leaves the computer.
//!
//! The download is the system's `curl` (macOS, Windows 10 and later, and Linux desktops have it),
//! so the app carries no HTTP or TLS stack of its own. `WORDCRAFT_WHISPER_MODEL` names another
//! model file in whisper.cpp's format (a larger one for better accuracy, or one fetched by hand
//! where the computer is offline); it's used as it is.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::dictation::Event;

const FILE: &str = "ggml-base.bin";
const URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin";
const SIZE: u64 = 147_951_465;
const SHA256: &str = "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe";

/// Where the model is kept.
fn dir() -> Option<PathBuf> {
    wordcraft_control_key::settings_dir().map(|d| d.join("Models"))
}

/// The model file, fetching it first if need be. `Err` says why there's none.
pub fn ensure(events: &Sender<Event>, stop: &AtomicBool) -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("WORDCRAFT_WHISPER_MODEL").filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return if p.is_file() { Ok(p) } else { Err(format!("WORDCRAFT_WHISPER_MODEL names {}, which isn't a file", p.display())) };
    }
    let dir = dir().ok_or("there's no settings folder to keep the speech model in")?;
    let path = dir.join(FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == SIZE) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    download(&path, events, stop)?;
    Ok(path)
}

fn download(path: &Path, events: &Sender<Event>, stop: &AtomicBool) -> Result<(), String> {
    let part = path.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let _ = events.send(Event::Downloading { done: 0, total: Some(SIZE) });
    let mut child = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error", "--proto", "=https", "--retry", "2", "--output"])
        .arg(&part)
        .arg(URL)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start curl to download the speech model ({e}); download {URL} and set WORDCRAFT_WHISPER_MODEL to it"))?;
    let status = loop {
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&part);
            return Err("the speech model download was cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(format!("the speech model download failed: {e}")),
        }
        let done = std::fs::metadata(&part).map_or(0, |m| m.len());
        let _ = events.send(Event::Downloading { done, total: Some(SIZE) });
        std::thread::sleep(Duration::from_millis(250));
    };
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        let _ = std::fs::remove_file(&part);
        let err = err.trim();
        return Err(if err.is_empty() {
            "the speech model didn't download (is the computer online?)".into()
        } else {
            format!("the speech model didn't download: {err}")
        });
    }
    let hash = sha256(&part).map_err(|e| format!("couldn't check the speech model: {e}"))?;
    if hash != SHA256 {
        let _ = std::fs::remove_file(&part);
        return Err("the downloaded speech model was damaged; try again".into());
    }
    std::fs::rename(&part, path).map_err(|e| format!("couldn't keep the speech model: {e}"))
}

fn sha256(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or(&[]));
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn sha256_of_a_file() {
        let dir = std::env::temp_dir().join(format!("wc-dictation-sha-{}", std::process::id()));
        std::fs::write(&dir, b"abc").unwrap();
        assert_eq!(super::sha256(&dir).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = std::fs::remove_file(&dir);
    }
}
