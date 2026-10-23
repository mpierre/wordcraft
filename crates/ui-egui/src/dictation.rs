//! Dictate (Home › Voice): runs the host's speech service while `tools.dictate` is on, types
//! what it hears through `dictate.insert`, and shows a small bar with the microphone level, what
//! the recogniser is doing, the language and Stop.
//!
//! The service ([`Services::dictation`](crate::Services)) is the desktop app's microphone and
//! speech model. It runs on its own threads and reports through a channel; this module only
//! starts and stops it and drains the channel each frame. Stopping lets it finish the phrase
//! being spoken, so the last words before Stop still land.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Rect, Sense, vec2};
use serde_json::json;
use wordcraft_engine::dictation::{AUTO, LANGUAGES, Status};

use crate::WordApp;
use crate::theme::Tokens;

/// What the speech service reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Fetching the speech model (bytes so far, of the total when known).
    Downloading {
        done: u64,
        total: Option<u64>,
    },
    /// Loading the model and opening the microphone.
    Starting,
    Listening,
    Transcribing,
    /// The microphone's loudness, 0…1.
    Level(f32),
    /// A recognised phrase, to type at the caret.
    Text(String),
    /// Dictation can't go on (no microphone, the model didn't download…); `Stopped` follows.
    Error(String),
    /// The service has finished; nothing more comes.
    Stopped,
}

/// What dictation is started with.
pub struct Request {
    /// A model language code, or `auto`.
    pub language: String,
    /// Wakes the UI when an event is sent.
    pub ctx: egui::Context,
}

/// A running speech service.
pub struct Running {
    pub events: Receiver<Event>,
    /// Set to ask the service to finish the phrase being spoken and stop.
    pub stop: Arc<AtomicBool>,
    /// The text before the caret, which the recogniser reads to carry on the sentence (and its
    /// spelling of names) instead of starting each phrase afresh. Kept up to date here.
    pub context: Arc<Mutex<String>>,
}

/// Starts the speech service.
pub type Start = Box<dyn Fn(Request) -> Running>;

/// Dictation in progress in the front end.
pub struct Session {
    running: Running,
    /// The language it was started with: changing it restarts the service.
    language: String,
    /// Stop has been asked for; waiting for the last phrase.
    stopping: bool,
}

/// How much text before the caret the recogniser gets.
const CONTEXT_CHARS: usize = 200;

/// The text before the caret in its paragraph, at most [`CONTEXT_CHARS`].
fn context(app: &WordApp) -> String {
    let (start, _) = app.session.sel.ordered();
    let Some(para) = app.session.doc.para_at(&start) else { return String::new() };
    let before = para.text.get(..start.off).unwrap_or("");
    let skip = before.chars().count().saturating_sub(CONTEXT_CHARS);
    before.chars().skip(skip).filter(|c| !c.is_control() && *c != wordcraft_engine::doc::para::OBJ).collect()
}

fn set_context(app: &WordApp, d: &Session) {
    let text = context(app);
    *d.running.context.lock().unwrap_or_else(PoisonError::into_inner) = text;
}

/// Per-frame bookkeeping: start or stop the service to match `tools.dictate`, and handle what it
/// reported.
pub fn poll(app: &mut WordApp, ctx: &egui::Context) {
    let language = app.session.dictation.language_for(app.session.editing_lang.as_deref());
    if let Some(d) = &mut app.dictation
        && !d.stopping
        && (!app.session.dictation.on || d.language != language)
    {
        d.running.stop.store(true, Ordering::Relaxed);
        d.stopping = true;
    }
    if app.session.dictation.on && app.dictation.is_none() {
        match &app.services.dictation {
            Some(start) => {
                let running = start(Request { language: language.clone(), ctx: ctx.clone() });
                let d = Session { running, language, stopping: false };
                set_context(app, &d);
                app.dictation = Some(d);
            }
            None => {
                app.session.dictation.on = false;
                app.session.dictation.status = Status::Off;
            }
        }
    }
    let mut finished = false;
    let mut typed = Vec::new();
    if let Some(d) = &app.dictation {
        loop {
            match d.running.events.try_recv() {
                Ok(Event::Downloading { done, total }) => app.session.dictation.status = Status::Downloading { done, total },
                Ok(Event::Starting) => app.session.dictation.status = Status::Starting,
                Ok(Event::Listening) => app.session.dictation.status = Status::Listening,
                Ok(Event::Transcribing) => app.session.dictation.status = Status::Transcribing,
                Ok(Event::Level(l)) => app.session.dictation.level = if l.is_finite() { l.clamp(0.0, 1.0) } else { 0.0 },
                Ok(Event::Text(t)) => typed.push(t),
                Ok(Event::Error(e)) => {
                    app.session.dictation.error = Some(e);
                }
                Ok(Event::Stopped) | Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
    }
    for t in typed {
        if let Err(e) = app.run("dictate.insert", json!({"text": t})) {
            // A read-only or protected document: say why, and stop listening.
            app.session.dictation.on = false;
            app.status(e);
        }
    }
    if let Some(d) = &app.dictation {
        set_context(app, d);
    }
    if let Some(e) = app.session.dictation.error.clone()
        && app.session.dictation.on
    {
        app.session.dictation.on = false;
        app.status(e);
    }
    if finished {
        let restart = app.dictation.as_ref().is_some_and(|d| d.stopping) && app.session.dictation.on;
        app.dictation = None;
        app.session.dictation.level = 0.0;
        if restart {
            // The language changed: the next frame starts it again in the new one.
            app.session.dictation.status = Status::Starting;
            ctx.request_repaint();
        } else {
            app.session.dictation.on = false;
            app.session.dictation.status = Status::Off;
        }
    }
    if app.dictation.is_some() {
        // The level meter moves while listening.
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }
}

/// The language picker's label for a setting.
fn language_name(code: &str) -> String {
    if code == AUTO {
        return tl!("Detect automatically").to_string();
    }
    LANGUAGES.iter().find(|(c, _)| *c == code).map_or_else(|| code.to_string(), |(_, n)| tl!(n).to_string())
}

/// The dictation bar, along the bottom of the page area while dictating.
pub fn show(app: &mut WordApp, ctx: &egui::Context) {
    if !app.session.dictation.on && app.dictation.is_none() {
        return;
    }
    let t = Tokens::get(ctx);
    let d = &app.session.dictation;
    let what = match &d.status {
        Status::Off => tl!("Stopping…").to_string(),
        Status::Downloading { done, total: Some(total) } if *total > 0 => {
            format!("{} {}%", tl!("Downloading the speech model…"), done.saturating_mul(100) / total)
        }
        Status::Downloading { done, .. } => format!("{} {} MB", tl!("Downloading the speech model…"), done / 1_000_000),
        Status::Starting => tl!("Starting…").to_string(),
        Status::Listening if app.dictation.as_ref().is_some_and(|d| d.stopping) => tl!("Stopping…").to_string(),
        Status::Listening => tl!("Listening…").to_string(),
        Status::Transcribing => tl!("Typing what you said…").to_string(),
    };
    let level = d.level;
    let listening = matches!(d.status, Status::Listening | Status::Transcribing);
    let current = d.language_for(app.session.editing_lang.as_deref());
    let mut run: Option<(&str, serde_json::Value)> = None;
    let bottom = app.canvas.canvas_rect.map_or(ctx.content_rect().center_bottom() + vec2(0.0, -48.0), |r| r.center_bottom() + vec2(0.0, -24.0));
    let window = egui::Window::new(tl!("Dictation"))
        .id(egui::Id::new("dictation-bar"))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .default_pos(bottom);
    window.show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // The microphone, with a ring that grows with the sound it hears.
            let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
            let color = if listening { t.accent } else { t.text_dim };
            if listening && level > 0.0 {
                let r = 8.0 + 4.0 * level.sqrt();
                ui.painter().circle_filled(rect.center(), r, color.gamma_multiply(0.25));
            }
            crate::icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(18.0, 18.0)), "dictate", color, color);
            ui.label(egui::RichText::new(what).color(t.text_dim));
            ui.add_space(4.0);
            egui::ComboBox::from_id_salt("dictation-language")
                .selected_text(language_name(&current))
                .height(320.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(current == AUTO, language_name(AUTO)).clicked() {
                        run = Some(("dictate.language", json!({"value": AUTO})));
                    }
                    ui.separator();
                    let mut names: Vec<(&str, String)> = LANGUAGES.iter().map(|(c, _)| (*c, language_name(c))).collect();
                    names.sort_by_key(|(_, n)| n.to_lowercase());
                    for (code, name) in names {
                        if ui.selectable_label(current == code, name).clicked() {
                            run = Some(("dictate.language", json!({"value": code})));
                        }
                    }
                })
                .response
                .on_hover_text(tl!("The language you speak"));
            if ui.button(tl!("Stop")).on_hover_text(tl!("Stop dictating")).clicked() {
                run = Some(("tools.dictate", json!({"on": false})));
            }
        });
    });
    if let Some((id, params)) = run
        && let Err(e) = app.run(id, params)
    {
        app.status(e);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    use wordcraft_engine::Session;

    use super::{Event, Request, Running};
    use crate::{Services, WordApp};

    /// A speech service that hears "Hello there." at once and "Last words." when stopped, and
    /// records the languages it was started in.
    fn fake(started: Arc<Mutex<Vec<String>>>) -> super::Start {
        Box::new(move |req: Request| {
            started.lock().unwrap().push(req.language.clone());
            let (tx, rx) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let stop2 = stop.clone();
            std::thread::spawn(move || {
                let _ = tx.send(Event::Starting);
                let _ = tx.send(Event::Listening);
                let _ = tx.send(Event::Level(0.6));
                let _ = tx.send(Event::Text(" Hello there.".into()));
                req.ctx.request_repaint();
                while !stop2.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let _ = tx.send(Event::Text(" Last words.".into()));
                let _ = tx.send(Event::Stopped);
                req.ctx.request_repaint();
            });
            Running { events: rx, stop, context: Arc::new(Mutex::new(String::new())) }
        })
    }

    fn harness(a: WordApp) -> egui_kittest::Harness<'static, WordApp> {
        egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            a,
        )
    }

    fn until(h: &mut egui_kittest::Harness<'static, WordApp>, what: &str, done: impl Fn(&WordApp) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done(h.state()) {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            h.step();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn text(a: &WordApp) -> String {
        a.session.doc.plain_text(wordcraft_doc::StoryRef::Body).trim_end().to_string()
    }

    #[test]
    fn dictation_types_what_it_hears_and_the_last_phrase_after_stop() {
        let started = Arc::new(Mutex::new(Vec::new()));
        let a = WordApp::new(Session::new(wordcraft_doc::Document::new()), Services { dictation: Some(fake(started.clone())), ..Default::default() });
        let mut h = harness(a);
        h.state_mut().run("tools.dictate", serde_json::json!({})).unwrap();
        until(&mut h, "the first phrase", |a| text(a) == "Hello there.");
        assert_eq!(*started.lock().unwrap(), vec!["en".to_string()], "started in the editing language (the interface's, English)");
        assert!(h.state().session.dictation.level > 0.5);
        h.state_mut().run("tools.dictate", serde_json::json!({})).unwrap();
        until(&mut h, "the service to stop", |a| a.dictation.is_none());
        assert_eq!(text(h.state()), "Hello there. Last words.");
        assert!(!h.state().session.dictation.on);
        assert_eq!(h.state().session.dictation.level, 0.0);
    }

    #[test]
    fn changing_the_language_restarts_the_service() {
        let started = Arc::new(Mutex::new(Vec::new()));
        let a = WordApp::new(Session::new(wordcraft_doc::Document::new()), Services { dictation: Some(fake(started.clone())), ..Default::default() });
        let mut h = harness(a);
        h.state_mut().run("tools.dictate", serde_json::json!({"language": "de"})).unwrap();
        until(&mut h, "German", |_| started.lock().unwrap().len() == 1);
        h.state_mut().run("dictate.language", serde_json::json!({"value": "fr"})).unwrap();
        until(&mut h, "French", |_| started.lock().unwrap().len() == 2);
        assert_eq!(*started.lock().unwrap(), vec!["de".to_string(), "fr".to_string()]);
        assert!(h.state().session.dictation.on, "still dictating");
    }

    #[test]
    fn errors_stop_dictation_and_say_why() {
        let svc: super::Start = Box::new(|req: Request| {
            let (tx, rx) = mpsc::channel();
            let _ = tx.send(Event::Error("there's no microphone".into()));
            let _ = tx.send(Event::Stopped);
            req.ctx.request_repaint();
            Running { events: rx, stop: Arc::new(AtomicBool::new(false)), context: Arc::new(Mutex::new(String::new())) }
        });
        let a = WordApp::new(Session::new(wordcraft_doc::Document::new()), Services { dictation: Some(svc), ..Default::default() });
        let mut h = harness(a);
        h.state_mut().run("tools.dictate", serde_json::json!({})).unwrap();
        until(&mut h, "the error", |a| !a.session.dictation.on && a.dictation.is_none());
        assert_eq!(h.state().session.dictation.error.as_deref(), Some("there's no microphone"));
    }

    #[test]
    fn without_a_service_dictate_is_greyed_out() {
        let mut a = WordApp::new(Session::new(wordcraft_doc::Document::new()), Services::default());
        assert!(a.run("tools.dictate", serde_json::json!({})).is_err());
    }
}
