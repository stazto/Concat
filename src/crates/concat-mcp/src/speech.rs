// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer Abdullah and Concat contributors

//! The voice tools: text-to-speech and voice cloning, as the window's
//! Speech dialog does them.
//!
//! Speech is not part of the Concat API: the window drives
//! [`concat_speech::Speech`] directly. These tools do the same, in this
//! process, whichever backend the editing tools go to. They read and write
//! only the models folder in Concat's app directory and the `audio/`
//! folder of a project the agent names, so they cannot collide with what
//! the window has open. What a synthesis writes is a WAV; putting it on the
//! timeline is `media_import` and `clip_add`, as for any other file.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use concat_api::{ApiError, AppDirs};
use concat_speech::tts::{Reference, SpeakRequest};
use concat_speech::{DownloadProgress, JobStage, Speech};
use serde_json::{Value, json};

/// One engine for the session: a model stays loaded between reads, as in
/// the window.
fn engine() -> &'static Arc<Speech> {
    static ENGINE: OnceLock<Arc<Speech>> = OnceLock::new();
    ENGINE.get_or_init(|| Arc::new(Speech::new()))
}

fn dirs() -> Result<AppDirs, ApiError> {
    AppDirs::locate().map_err(|error| ApiError::failed(format!("Concat's app folder: {error}")))
}

/// `speech_status`: every model with whether it is downloaded, and every
/// voice with the family that reads it.
pub fn status() -> Result<Value, ApiError> {
    let status = Speech::status(&dirs()?).map_err(ApiError::failed)?;
    let mut value =
        serde_json::to_value(status).map_err(|error| ApiError::failed(error.to_string()))?;
    value["cloneVoices"] = json!({
        "pocket": concat_speech::tts::POCKET_CLONE,
        "chatterbox": concat_speech::tts::CHATTERBOX_CLONE,
        "note": "A clone voice reads in the voice of the recording given as `referencePath`. Chatterbox needs at least five seconds of clean speech; Pocket uses up to ten.",
    });
    Ok(value)
}

/// `speech_model_download`: fetches one model into the models folder.
/// Blocks until it is on disk, reporting bytes as they arrive.
pub fn download(
    arguments: Value,
    cancelled: &AtomicBool,
    progress: Option<&dyn Fn(f64, f64, String)>,
) -> Result<Value, ApiError> {
    let id = arguments
        .get("modelId")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::invalid("invalid arguments: missing field `modelId`"))?
        .to_owned();
    let dirs = dirs()?;
    let speech = Arc::clone(engine());
    let (tell, heard) = mpsc::channel::<DownloadProgress>();
    let worker = {
        let id = id.clone();
        let speech = Arc::clone(&speech);
        std::thread::spawn(move || speech.download_model(&dirs, &id, move |p| drop(tell.send(p))))
    };
    follow(
        &worker,
        cancelled,
        || speech.cancel_download(),
        || {
            while let Ok(p) = heard.try_recv() {
                if let Some(report) = progress {
                    let what = if p.unpacking {
                        "unpacking"
                    } else {
                        "downloading"
                    };
                    report(
                        p.received as f64,
                        p.total.max(p.received) as f64,
                        format!("{what} {}", p.id),
                    );
                }
            }
        },
    );
    worker
        .join()
        .map_err(|_| ApiError::failed("the download thread panicked"))?
        .map_err(ApiError::failed)?;
    Ok(json!({ "modelId": id, "downloaded": true }))
}

/// `speech_synthesize`: reads `text` aloud into `<project>/audio/` and
/// returns the WAV's path and length.
pub fn synthesize(
    arguments: Value,
    cancelled: &AtomicBool,
    progress: Option<&dyn Fn(f64, f64, String)>,
) -> Result<Value, ApiError> {
    let field = |name: &str| arguments.get(name);
    let text = field("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let model_id = field("modelId")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::invalid("invalid arguments: missing field `modelId`"))?
        .to_owned();
    let voice = field("voice")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::invalid("invalid arguments: missing field `voice`"))?
        as i32;
    let project = field("project")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::invalid("invalid arguments: missing field `project`"))?
        .to_owned();
    let reference = field("referencePath")
        .and_then(Value::as_str)
        .map(|path| Reference {
            path: path.to_owned(),
            start: field("referenceStart")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
        });
    if concat_speech::tts::is_clone(voice) && reference.is_none() {
        return Err(ApiError::invalid(
            "a clone voice reads in the voice of a recording: give `referencePath`",
        ));
    }
    let request = SpeakRequest {
        model_id,
        voice,
        reference,
        text,
        speed: field("speed").and_then(Value::as_f64).unwrap_or(1.0) as f32,
        pauses: field("pauses").and_then(Value::as_f64).map(|v| v as f32),
        steps: field("steps").and_then(Value::as_i64).map(|v| v as i32),
        project,
    };

    let dirs = dirs()?;
    let speech = Arc::clone(engine());
    let (tell, heard) = mpsc::channel::<JobStage>();
    let worker = {
        let speech = Arc::clone(&speech);
        std::thread::spawn(move || {
            speech.speak(&dirs, &request, move |stage| {
                // The receiver is gone only once the call has ended.
                let _ = tell.send(stage);
            })
        })
    };
    follow(
        &worker,
        cancelled,
        || speech.cancel(),
        || {
            while let Ok(stage) = heard.try_recv() {
                if let Some(report) = progress {
                    let (done, what) = match stage {
                        JobStage::Reading => (0.0, "reading the voice recording".to_owned()),
                        JobStage::Loading => (0.0, "loading the voice model".to_owned()),
                        JobStage::Working { done, .. } => (f64::from(done), "speaking".to_owned()),
                    };
                    report(done, 1.0, what);
                }
            }
        },
    );
    let result = worker
        .join()
        .map_err(|_| ApiError::failed("the speech thread panicked"))?
        .map_err(|error| ApiError::failed(error.to_string()))?;
    Ok(json!({
        "path": result.path,
        "duration": result.duration,
        "next": "Put it on the timeline with media_import and clip_add.",
    }))
}

/// Waits for a worker, passing its progress on and stopping it when the
/// client cancels.
fn follow<T>(
    worker: &std::thread::JoinHandle<T>,
    cancelled: &AtomicBool,
    cancel: impl Fn(),
    mut drain: impl FnMut(),
) {
    let mut asked = false;
    while !worker.is_finished() {
        drain();
        if !asked && cancelled.load(Ordering::Relaxed) {
            cancel();
            asked = true;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    drain();
}

/// The arguments `speech_status` takes: none.
pub fn status_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// The arguments `speech_model_download` takes.
pub fn download_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "modelId": { "type": "string", "description": "A model id from speech_status, e.g. \"kokoro-int8-multi-lang-v1_0\"." },
        },
        "required": ["modelId"],
    })
}

/// The arguments `speech_synthesize` takes.
pub fn synthesize_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "project": { "type": "string", "description": "A Concat project folder (from project_create or project_open); the WAV lands in its audio/ folder." },
            "text": { "type": "string", "description": "What to say." },
            "modelId": { "type": "string", "description": "A downloaded model id from speech_status." },
            "voice": { "type": "integer", "description": "A voice id from speech_status that belongs to the model's family, or that family's clone voice (cloneVoices)." },
            "referencePath": { "type": "string", "description": "For a clone voice: the audio or video file whose voice is read in." },
            "referenceStart": { "type": "number", "minimum": 0, "default": 0, "description": "Seconds into the reference where the voice is heard." },
            "speed": { "type": "number", "minimum": 0.5, "maximum": 2, "default": 1, "description": "Speaking rate; Chatterbox reads at its own pace." },
            "pauses": { "type": "number", "minimum": 0, "maximum": 1, "description": "Breath between sentences (Kokoro and Pocket): 0 runs them together, 0.2 is the engine's own, 1 is long." },
            "steps": { "type": "integer", "minimum": 1, "maximum": 16, "description": "Pocket only: more is a cleaner voice and a longer wait (5 by default)." },
        },
        "required": ["project", "text", "modelId", "voice"],
    })
}
