#[cfg(feature = "audio-transcription")]
use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(feature = "audio-transcription")]
use anyhow::Context;
use anyhow::{Result, bail};
#[cfg(not(feature = "audio-transcription"))]
use std::path::PathBuf;
#[cfg(feature = "audio-transcription")]
use tokio::process::Command;
#[cfg(feature = "audio-transcription")]
use transcribe_rs::{
    SpeechModel, TranscribeOptions,
    onnx::{Quantization, parakeet::ParakeetModel},
};
#[cfg(feature = "audio-transcription")]
use uuid::Uuid;

use crate::models::AttachmentTranscript;

#[cfg(feature = "audio-transcription")]
const HANDY_MODEL_DIR_NAME: &str = "parakeet-tdt-0.6b-v3-int8";

#[cfg(feature = "audio-transcription")]
pub fn detect_handy_parakeet_model_dir() -> Option<PathBuf> {
    handy_model_roots()
        .into_iter()
        .map(|root| root.join(HANDY_MODEL_DIR_NAME))
        .find(|candidate| is_valid_parakeet_model_dir(candidate))
}

#[cfg(not(feature = "audio-transcription"))]
pub fn detect_handy_parakeet_model_dir() -> Option<PathBuf> {
    None
}

#[cfg(not(feature = "audio-transcription"))]
pub async fn transcribe_audio_file(
    _model_dir: PathBuf,
    _source_path: PathBuf,
    _scratch_dir: PathBuf,
) -> Result<AttachmentTranscript> {
    bail!("audio transcription is disabled in this build")
}

#[cfg(feature = "audio-transcription")]
pub async fn transcribe_audio_file(
    model_dir: PathBuf,
    source_path: PathBuf,
    scratch_dir: PathBuf,
) -> Result<AttachmentTranscript> {
    let wav_path = scratch_dir.join(format!("{}.wav", Uuid::now_v7()));
    convert_audio_to_wav(&source_path, &wav_path).await?;

    let wav_for_transcription = wav_path.clone();
    let transcript_result = tokio::task::spawn_blocking(move || -> Result<AttachmentTranscript> {
        let mut model = ParakeetModel::load(&model_dir, &Quantization::Int8)
            .with_context(|| format!("failed to load Handy model from {}", model_dir.display()))?;
        let result = model
            .transcribe_file(&wav_for_transcription, &TranscribeOptions::default())
            .with_context(|| format!("failed to transcribe {}", wav_for_transcription.display()))?;
        let text = result.text.trim().to_string();
        if text.is_empty() {
            bail!("transcript is empty");
        }
        Ok(AttachmentTranscript {
            engine: "Handy Parakeet".to_string(),
            text,
        })
    })
    .await
    .context("audio transcription task join failed")?;

    let _ = fs::remove_file(&wav_path);
    transcript_result
}

#[cfg(feature = "audio-transcription")]
async fn convert_audio_to_wav(source_path: &Path, wav_path: &Path) -> Result<()> {
    let output = Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(source_path)
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("16000")
        .arg("-c:a")
        .arg("pcm_s16le")
        .arg(wav_path)
        .output()
        .await
        .with_context(|| format!("failed to spawn ffmpeg for {}", source_path.display()))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        bail!("ffmpeg exited with status {}", output.status);
    }
    bail!("ffmpeg exited with status {}: {stderr}", output.status);
}

#[cfg(feature = "audio-transcription")]
fn handy_model_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(
            PathBuf::from(&appdata)
                .join("com.pais.handy")
                .join("models"),
        );
    }
    if let Some(local_appdata) = std::env::var_os("LOCALAPPDATA") {
        roots.push(
            PathBuf::from(local_appdata)
                .join("Handy")
                .join("resources")
                .join("models"),
        );
    }
    roots
}

#[cfg(feature = "audio-transcription")]
fn is_valid_parakeet_model_dir(dir: &Path) -> bool {
    [
        "encoder-model.int8.onnx",
        "decoder_joint-model.int8.onnx",
        "nemo128.onnx",
        "vocab.txt",
    ]
    .iter()
    .all(|name| dir.join(name).is_file())
}

#[cfg(all(test, feature = "audio-transcription"))]
mod tests {
    use super::*;

    #[test]
    fn rejects_incomplete_model_dir() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!is_valid_parakeet_model_dir(temp.path()));
    }
}
