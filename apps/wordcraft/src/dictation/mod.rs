//! Dictate's speech service (#785, #680, #3214): the microphone ([`audio`]), cut into phrases at
//! the speaker's pauses ([`segmenter`]), each turned into text by a local Whisper model through
//! whisper.cpp ([`model`]; Metal on Apple silicon, the CPU elsewhere), with the text it invents
//! from scraps of sound dropped ([`filter`]). Speech never leaves the
//! computer; the only network use is fetching the model the first time.
//!
//! Everything runs on a worker thread that reports to the UI through the channel in
//! [`wordcraft_ui_egui::dictation::Running`]: progress, the microphone level, and each phrase's
//! text, which the UI types with `dictate.insert`. Stop lets the worker finish the phrase being
//! spoken first. The loaded model is kept for the next Dictate, so only the first one waits.

mod audio;
mod filter;
mod model;
mod segmenter;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
use wordcraft_ui_egui::dictation::{Event, Request, Running, Start};

use segmenter::{Phrase, RATE, Resampler, Segmenter, rms};

/// The model loaded by an earlier Dictate (its file and context).
static MODEL: Mutex<Option<(PathBuf, Arc<WhisperContext>)>> = Mutex::new(None);

/// How long the microphone may send only digital silence before we say it can't be heard (macOS
/// sends zeros when the app isn't allowed to use it).
const SILENT_MIC: Duration = Duration::from_secs(4);
/// The shortest sound the model is given (it wants at least a second).
const MIN_SAMPLES: usize = RATE * 11 / 10;

/// The service for [`wordcraft_ui_egui::Services::dictation`].
pub fn hook() -> Option<Start> {
    whisper_rs::install_logging_hooks();
    Some(Box::new(start))
}

fn start(req: Request) -> Running {
    let (tx, rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let context = Arc::new(Mutex::new(String::new()));
    let (stop2, context2, ctx) = (stop.clone(), context.clone(), req.ctx.clone());
    let language = req.language;
    let events = Events { tx: tx.clone(), ctx: ctx.clone() };
    let spawned = std::thread::Builder::new().name("dictation".into()).spawn(move || {
        if let Err(e) = run(&events, &language, &stop2, &context2) {
            log::warn!("dictation: {e}");
            events.send(Event::Error(e));
        }
        events.send(Event::Stopped);
    });
    if let Err(e) = spawned {
        let _ = tx.send(Event::Error(format!("couldn't start dictation: {e}")));
        let _ = tx.send(Event::Stopped);
        ctx.request_repaint();
    }
    Running { events: rx, stop, context }
}

/// The channel to the UI, which it wakes.
struct Events {
    tx: Sender<Event>,
    ctx: egui::Context,
}

impl Events {
    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
}

/// The model, loading it unless an earlier Dictate did.
fn load(path: &std::path::Path) -> Result<Arc<WhisperContext>, String> {
    let mut cached = MODEL.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((p, ctx)) = cached.as_ref()
        && p == path
    {
        return Ok(ctx.clone());
    }
    let path_str = path.to_str().ok_or("the speech model's path isn't valid text")?;
    let ctx =
        WhisperContext::new_with_params(path_str, WhisperContextParameters::default()).map_err(|e| format!("the speech model didn't load: {e}"))?;
    let ctx = Arc::new(ctx);
    *cached = Some((path.to_path_buf(), ctx.clone()));
    Ok(ctx)
}

fn run(events: &Events, language: &str, stop: &AtomicBool, context: &Mutex<String>) -> Result<(), String> {
    events.send(Event::Starting);
    let path = model::ensure(&events.tx, stop)?;
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    events.send(Event::Starting);
    let model = load(&path)?;
    let mut state = model.create_state().map_err(|e| format!("the speech model didn't start: {e}"))?;
    let mic = audio::open()?;
    let mut resampler = Resampler::new(mic.rate);
    let mut segmenter = Segmenter::default();
    events.send(Event::Listening);
    let opened = Instant::now();
    let mut heard_anything = false;
    let mut last_level = Instant::now();
    let mut phrase_done = |phrase: Phrase| -> Result<(), String> {
        events.send(Event::Transcribing);
        let voiced = phrase.voiced_secs();
        let before = context.lock().unwrap_or_else(PoisonError::into_inner).clone();
        // Shown the text before the caret, a short scrap of sound is "heard" as that text
        // again or a sentence to follow it (`filter`).
        let prompt = if voiced >= filter::PROMPT_SECS { before.as_str() } else { "" };
        let text = transcribe(&mut state, phrase.samples, language, prompt)?;
        if filter::heard(&text, &before, voiced) {
            events.send(Event::Text(text));
        }
        events.send(Event::Listening);
        Ok(())
    };
    loop {
        if stop.load(Ordering::Relaxed) {
            if let Some(p) = segmenter.finish() {
                phrase_done(p)?;
            }
            return Ok(());
        }
        // Nothing at all from the microphone counts as silence too, so a recorder that never
        // sends doesn't leave dictation listening forever.
        let chunk = match mic.chunks.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return Err("the microphone stopped".into()),
        };
        heard_anything |= chunk.as_ref().is_some_and(|c| c.iter().any(|s| *s != 0.0));
        if !heard_anything && opened.elapsed() > SILENT_MIC {
            return Err(if cfg!(target_os = "macos") {
                "WordCraft can't hear the microphone: allow it in System Settings › Privacy & Security › Microphone".into()
            } else {
                "the microphone sends no sound: check that it's connected and not muted".into()
            });
        }
        let Some(chunk) = chunk else { continue };
        let samples = resampler.push(&chunk);
        if last_level.elapsed() >= Duration::from_millis(80) {
            events.send(Event::Level((rms(&samples) * 8.0).min(1.0)));
            last_level = Instant::now();
        }
        for phrase in segmenter.push(&samples) {
            phrase_done(phrase)?;
        }
    }
}

/// One phrase as text.
fn transcribe(state: &mut whisper_rs::WhisperState, mut phrase: Vec<f32>, language: &str, prompt: &str) -> Result<String, String> {
    if phrase.len() < MIN_SAMPLES {
        phrase.resize(MIN_SAMPLES, 0.0);
    }
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
    params.set_n_threads(i32::try_from(threads).unwrap_or(2));
    params.set_language(Some(language));
    params.set_translate(false);
    params.set_no_timestamps(true);
    params.set_no_context(true);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    if !prompt.trim().is_empty() {
        // The text before the caret: the phrase carries on its sentence, names keep their spelling.
        params.set_initial_prompt(prompt);
    }
    state.full(params, &phrase).map_err(|e| format!("the speech model failed: {e}"))?;
    let mut text = String::new();
    for segment in state.as_iter() {
        // Breaths and room noise the model thinks were words.
        if segment.no_speech_probability() > 0.8 {
            continue;
        }
        if let Ok(t) = segment.to_str_lossy() {
            text.push_str(&t);
        }
    }
    Ok(text)
}
