//! Whisper.cpp voice-transcription fallback for the `/voice` slash command.
//!
//! The frontend's primary path is the browser's built-in `SpeechRecognition`
//! API. When that's unavailable (Linux/Firefox), or fails, the frontend can
//! POST the captured audio blob (base64 wav) to this command, which shells
//! out to a locally-installed `whisper-cli` binary (Whisper.cpp's CLI).
//!
//! Discovery rules:
//!   - `whisper-cli` must be on PATH; if not we return an error that the
//!     frontend can surface as "install whisper.cpp".
//!   - Model file: `~/.cortex/models/ggml-base.en.bin` if present, else
//!     `ggml-tiny.en.bin`. If neither exists we return a clear error.
//!
//! The audio is written to `<temp>/cortex-voice-<uuid>.wav`, transcribed,
//! and the temp file is removed on the way out (success *and* failure).

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::fs;
use std::path::PathBuf;

/// Cap on the decoded audio payload. A `/voice` utterance is a short PCM wav;
/// 64 MiB is already far beyond anything a microphone capture produces, but
/// bounds memory + temp-disk use against a runaway/hostile caller. The base64
/// input is rejected first (before allocation) using the ~4/3 expansion ratio.
const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;

fn model_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    let dir = home.join(".cortex").join("models");
    let base = dir.join("ggml-base.en.bin");
    if base.exists() {
        return Ok(base);
    }
    let tiny = dir.join("ggml-tiny.en.bin");
    if tiny.exists() {
        return Ok(tiny);
    }
    Err(format!(
        "no whisper model at {} (expected ggml-base.en.bin or ggml-tiny.en.bin)",
        dir.display()
    ))
}

#[tauri::command]
pub async fn voice_transcribe(audio_b64: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        // 1. Locate the whisper CLI.
        let cli = which::which("whisper-cli").map_err(|_| "whisper-cli not installed".to_string())?;

        // 2. Locate a model (base preferred, tiny fallback).
        let model = model_path()?;

        // 3. Decode the base64 wav to a uniquely-named temp file. Reject an
        //    oversized payload up front (base64 expands ~4/3, so cap the encoded
        //    length) so a hostile/buggy caller can't OOM or fill the temp disk.
        if audio_b64.len() / 4 * 3 > MAX_AUDIO_BYTES {
            return Err("audio payload too large".into());
        }
        let bytes = B64
            .decode(audio_b64.as_bytes())
            .map_err(|e| format!("invalid base64 audio: {e}"))?;
        if bytes.is_empty() {
            return Err("empty audio payload".into());
        }
        if bytes.len() > MAX_AUDIO_BYTES {
            return Err("audio payload too large".into());
        }
        let tmp = std::env::temp_dir().join(format!("cortex-voice-{}.wav", uuid::Uuid::new_v4()));
        fs::write(&tmp, &bytes).map_err(|e| format!("write temp wav: {e}"))?;

        // 4. Run whisper-cli. `-nt` strips timestamps, `-ng` disables GPU.
        let output = crate::sys::no_window(&cli)
            .arg("-m")
            .arg(&model)
            .arg("-f")
            .arg(&tmp)
            .arg("-nt")
            .arg("-ng")
            .output();

        // 5. Always remove the temp file, regardless of how the run went.
        let _ = fs::remove_file(&tmp);

        let output = output.map_err(|e| format!("whisper-cli spawn failed: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "whisper-cli exited with {}: {}",
                output.status,
                stderr.trim()
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(text)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}
