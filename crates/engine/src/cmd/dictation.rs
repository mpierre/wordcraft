//! Dictate (Home › Voice): turning dictation on and off, its language, and typing what was said.
//!
//! The state is [`crate::dictation::Dictation`]; the desktop app's speech service listens while
//! it's on and hands each recognised phrase to `dictate.insert`.

use serde_json::{Value, json};

use super::sel_result;
use crate::dictation::{self, AUTO, LANGUAGES, MAX_PHRASE, Piece, Status};
use crate::{CmdError, CmdResult, CommandSpec, Session, p};

const LOC: &str = "Home › Voice";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("tools.dictate", "Dictate", LOC, dictate)
            .params(r#"{"on"?: bool, "language"?: string ("auto" or a code such as "en", "sv")}"#)
            .when(available)
            .pure(),
        CommandSpec::new("dictate.language", "Dictation Language", LOC, language)
            .params(r#"{"value": string ("auto", a code such as "en", or "" to follow the editing language)}"#)
            .pure(),
        // The typing goes through `text.insert` and `text.newParagraph`, which take the undo
        // step and check Restrict Editing themselves.
        CommandSpec::new("dictate.insert", "Type Dictated Text", LOC, insert).params(r#"{"text": string}"#).pure(),
        CommandSpec::new("dictate.status", "Dictation Status", LOC, |s, _| Ok(status(s))).pure(),
    ]
}

fn available(s: &Session) -> Option<&'static str> {
    (!s.dictation.available).then_some("dictation needs the desktop app")
}

/// The state for agents and the front end.
fn status(s: &Session) -> Value {
    let d = &s.dictation;
    json!({
        "on": d.on,
        "available": d.available,
        "language": d.language_for(s.editing_lang.as_deref()),
        "followsEditingLanguage": d.language.is_none(),
        "status": d.status.to_json(),
        "error": d.error,
    })
}

/// A language setting from params: a model code, `auto`, or empty (follow the editing language).
fn parse_language(value: &str) -> Result<Option<String>, CmdError> {
    let v = value.trim();
    if v.is_empty() {
        return Ok(None);
    }
    if v.eq_ignore_ascii_case(AUTO) {
        return Ok(Some(AUTO.into()));
    }
    dictation::model_language(v)
        .map(|c| Some(c.to_string()))
        .ok_or_else(|| CmdError::Params(format!("{v:?} isn't a language dictation knows; use \"auto\" or one of {}", codes())))
}

fn codes() -> String {
    LANGUAGES.iter().map(|(c, _)| *c).collect::<Vec<_>>().join(", ")
}

/// Turn dictation on or off (toggles without `on`).
fn dictate(s: &mut Session, v: &Value) -> CmdResult {
    if let Some(l) = p::str(v, "language") {
        s.dictation.language = parse_language(l)?;
    }
    let on = p::bool(v, "on").unwrap_or(!s.dictation.on);
    if on && !s.dictation.on {
        s.dictation.error = None;
        s.dictation.status = Status::Starting;
    }
    if !on {
        s.dictation.status = Status::Off;
        s.dictation.level = 0.0;
    }
    s.dictation.on = on;
    Ok(status(s))
}

fn language(s: &mut Session, v: &Value) -> CmdResult {
    s.dictation.language = parse_language(p::req_str(v, "value")?)?;
    Ok(status(s))
}

/// The characters just before and after the caret (around the selection, which typing
/// replaces), each `None` at the paragraph's start or end, or outside the same paragraph.
fn chars_around(s: &Session) -> (Option<char>, Option<char>) {
    let (start, end) = s.sel.ordered();
    let before = s.doc.para_at(&start).and_then(|p| p.text.get(..start.off)?.chars().last());
    let after = (end.story == start.story && end.path == start.path)
        .then(|| s.doc.para_at(&end).and_then(|p| p.text.get(end.off..)?.chars().next()))
        .flatten();
    (before, after)
}

/// Type a recognised phrase at the caret.
fn insert(s: &mut Session, v: &Value) -> CmdResult {
    let text = p::req_str(v, "text")?;
    if text.len() > MAX_PHRASE {
        return Err(CmdError::Params(format!("a dictated phrase is at most {MAX_PHRASE} bytes")));
    }
    // The spoken commands are English words; with "auto" they're likely English too.
    let english = matches!(s.dictation.language_for(s.editing_lang.as_deref()).as_str(), "en" | AUTO);
    let mut typed = String::new();
    let (before, after) = chars_around(s);
    for piece in dictation::pieces(text, before, after, english) {
        match piece {
            Piece::Text(t) => {
                s.run("text.insert", &json!({"text": t, "raw": true}))?;
                typed.push_str(&t);
            }
            Piece::NewParagraph => {
                s.run("text.newParagraph", &json!({}))?;
                typed.push('\n');
            }
        }
    }
    let mut r = sel_result(s)?;
    if let Value::Object(m) = &mut r {
        m.insert("typed".into(), Value::String(typed));
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    fn text(s: &Session) -> String {
        s.doc.plain_text(wordcraft_doc::StoryRef::Body)
    }

    #[test]
    fn dictate_needs_a_speech_service() {
        let mut s = Session::new(wordcraft_doc::Document::new());
        assert!(s.run("tools.dictate", &json!({})).is_err(), "greyed out without the desktop app");
        s.dictation.available = true;
        let r = s.run("tools.dictate", &json!({})).unwrap();
        assert_eq!(r["on"], true);
        assert_eq!(r["status"]["state"], "starting");
        let r = s.run("tools.dictate", &json!({})).unwrap();
        assert_eq!(r["on"], false);
        assert_eq!(s.run("tools.dictate", &json!({"on": false})).unwrap()["on"], false);
    }

    #[test]
    fn language_follows_the_editing_language_until_chosen() {
        let mut s = Session::new(wordcraft_doc::Document::new());
        s.dictation.available = true;
        s.editing_lang = Some("sv-SE".into());
        assert_eq!(s.run("dictate.status", &json!({})).unwrap()["language"], "sv");
        assert_eq!(s.run("dictate.language", &json!({"value": "de-AT"})).unwrap()["language"], "de");
        assert_eq!(s.run("tools.dictate", &json!({"language": "AUTO"})).unwrap()["language"], "auto");
        assert!(s.run("dictate.language", &json!({"value": "zz"})).is_err());
        assert_eq!(s.run("dictate.language", &json!({"value": ""})).unwrap()["language"], "sv");
    }

    #[test]
    fn dictated_phrases_are_typed_with_spaces_and_commands() {
        let mut s = Session::new(wordcraft_doc::Document::new());
        s.run("dictate.insert", &json!({"text": " Dear Sam, new paragraph."})).unwrap();
        s.run("dictate.insert", &json!({"text": " thank you for the letter."})).unwrap();
        s.run("dictate.insert", &json!({"text": " It arrived today."})).unwrap();
        assert_eq!(text(&s).trim_end(), "Dear Sam,\nThank you for the letter. It arrived today.");
        // Each phrase is undone on its own.
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(text(&s).trim_end(), "Dear Sam,\nThank you for the letter.");
    }

    #[test]
    fn dictating_in_front_of_text_keeps_a_space() {
        let mut s = Session::new(wordcraft_doc::Document::new());
        s.run("text.insert", &json!({"text": "The end"})).unwrap();
        s.run("caret.home", &json!({})).unwrap();
        s.run("dictate.insert", &json!({"text": " Hello there."})).unwrap();
        assert_eq!(text(&s).trim_end(), "Hello there. The end");
    }

    #[test]
    fn hostile_phrases_are_refused_or_ignored() {
        let mut s = Session::new(wordcraft_doc::Document::new());
        assert!(s.run("dictate.insert", &json!({"text": "x".repeat(super::MAX_PHRASE + 1)})).is_err());
        s.run("dictate.insert", &json!({"text": "[BLANK_AUDIO]"})).unwrap();
        s.run("dictate.insert", &json!({"text": ""})).unwrap();
        assert_eq!(text(&s).trim_end(), "");
    }
}
