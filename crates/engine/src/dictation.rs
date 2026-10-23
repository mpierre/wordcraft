//! Dictate (Home › Voice): speech typed into the document at the caret.
//!
//! The engine keeps only what the commands control and what the front end shows: whether
//! dictation is on, the language spoken, and how the recogniser is getting on. The microphone and
//! the speech model live with the desktop app (a platform service, like printing); it hands each
//! phrase it recognised to `dictate.insert`, which types it like the keyboard does, so undo,
//! Restrict Editing and Track Changes treat dictated text as typed text.
//!
//! A phrase is shaped before it's typed ([`pieces`]): the recogniser's non-speech tags
//! (`[BLANK_AUDIO]`, `(music)`) are dropped, a space separates it from the word before the caret,
//! and the spoken commands "new line" and "new paragraph" start a new paragraph.

use serde_json::{Value, json};
use wordcraft_doc::para::{COLUMN_BREAK, PAGE_BREAK};

/// The languages the speech model understands (Whisper's codes and English names), in the
/// model's own order (roughly by how much training speech it had).
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("zh", "Chinese"),
    ("de", "German"),
    ("es", "Spanish"),
    ("ru", "Russian"),
    ("ko", "Korean"),
    ("fr", "French"),
    ("ja", "Japanese"),
    ("pt", "Portuguese"),
    ("tr", "Turkish"),
    ("pl", "Polish"),
    ("ca", "Catalan"),
    ("nl", "Dutch"),
    ("ar", "Arabic"),
    ("sv", "Swedish"),
    ("it", "Italian"),
    ("id", "Indonesian"),
    ("hi", "Hindi"),
    ("fi", "Finnish"),
    ("vi", "Vietnamese"),
    ("he", "Hebrew"),
    ("uk", "Ukrainian"),
    ("el", "Greek"),
    ("ms", "Malay"),
    ("cs", "Czech"),
    ("ro", "Romanian"),
    ("da", "Danish"),
    ("hu", "Hungarian"),
    ("ta", "Tamil"),
    ("no", "Norwegian"),
    ("th", "Thai"),
    ("ur", "Urdu"),
    ("hr", "Croatian"),
    ("bg", "Bulgarian"),
    ("lt", "Lithuanian"),
    ("la", "Latin"),
    ("mi", "Maori"),
    ("ml", "Malayalam"),
    ("cy", "Welsh"),
    ("sk", "Slovak"),
    ("te", "Telugu"),
    ("fa", "Persian"),
    ("lv", "Latvian"),
    ("bn", "Bengali"),
    ("sr", "Serbian"),
    ("az", "Azerbaijani"),
    ("sl", "Slovenian"),
    ("kn", "Kannada"),
    ("et", "Estonian"),
    ("mk", "Macedonian"),
    ("br", "Breton"),
    ("eu", "Basque"),
    ("is", "Icelandic"),
    ("hy", "Armenian"),
    ("ne", "Nepali"),
    ("mn", "Mongolian"),
    ("bs", "Bosnian"),
    ("kk", "Kazakh"),
    ("sq", "Albanian"),
    ("sw", "Swahili"),
    ("gl", "Galician"),
    ("mr", "Marathi"),
    ("pa", "Punjabi"),
    ("si", "Sinhala"),
    ("km", "Khmer"),
    ("sn", "Shona"),
    ("yo", "Yoruba"),
    ("so", "Somali"),
    ("af", "Afrikaans"),
    ("oc", "Occitan"),
    ("ka", "Georgian"),
    ("be", "Belarusian"),
    ("tg", "Tajik"),
    ("sd", "Sindhi"),
    ("gu", "Gujarati"),
    ("am", "Amharic"),
    ("yi", "Yiddish"),
    ("lo", "Lao"),
    ("uz", "Uzbek"),
    ("fo", "Faroese"),
    ("ht", "Haitian Creole"),
    ("ps", "Pashto"),
    ("tk", "Turkmen"),
    ("nn", "Nynorsk"),
    ("mt", "Maltese"),
    ("sa", "Sanskrit"),
    ("lb", "Luxembourgish"),
    ("my", "Myanmar"),
    ("bo", "Tibetan"),
    ("tl", "Tagalog"),
    ("mg", "Malagasy"),
    ("as", "Assamese"),
    ("tt", "Tatar"),
    ("haw", "Hawaiian"),
    ("ln", "Lingala"),
    ("ha", "Hausa"),
    ("ba", "Bashkir"),
    ("jw", "Javanese"),
    ("su", "Sundanese"),
    ("yue", "Cantonese"),
];

/// The language setting that lets the model tell which language is spoken.
pub const AUTO: &str = "auto";

/// Longest phrase `dictate.insert` types at once (the recogniser's phrases are a few hundred
/// characters at most).
pub const MAX_PHRASE: usize = 4_000;

/// The model's code for a language tag (`pt-BR` → `pt`, `nb` → `no`), if it understands it.
pub fn model_language(tag: &str) -> Option<&'static str> {
    let primary = tag.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    let primary = match primary.as_str() {
        // Norwegian Bokmål is the model's "Norwegian"; Filipino its "Tagalog".
        "nb" => "no",
        "fil" => "tl",
        "jv" => "jw",
        p => p,
    };
    LANGUAGES.iter().find(|(code, _)| *code == primary).map(|(code, _)| *code)
}

/// How the recogniser is getting on; the front end sets it from what the platform service says.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    /// Dictation is off.
    #[default]
    Off,
    /// Fetching the speech model the first time (bytes so far, of the total when known).
    Downloading { done: u64, total: Option<u64> },
    /// Loading the speech model and opening the microphone.
    Starting,
    /// Listening for speech.
    Listening,
    /// Turning a phrase into text.
    Transcribing,
}

impl Status {
    pub fn to_json(&self) -> Value {
        match self {
            Status::Off => json!({"state": "off"}),
            Status::Downloading { done, total } => json!({"state": "downloading", "done": done, "total": total}),
            Status::Starting => json!({"state": "starting"}),
            Status::Listening => json!({"state": "listening"}),
            Status::Transcribing => json!({"state": "transcribing"}),
        }
    }
}

/// Dictation's state in the session.
#[derive(Clone, Debug, Default)]
pub struct Dictation {
    /// Whether this front end can dictate (the desktop app with a speech service); without it
    /// `tools.dictate` is greyed out.
    pub available: bool,
    /// Whether dictation is on (the ribbon button is pressed).
    pub on: bool,
    /// The language spoken: a code from [`LANGUAGES`], or [`AUTO`]. `None` follows the editing
    /// language.
    pub language: Option<String>,
    /// What the recogniser is doing.
    pub status: Status,
    /// The microphone's loudness, 0…1, for the level meter.
    pub level: f32,
    /// Why dictation stopped, if it stopped on its own (no microphone, no model…).
    pub error: Option<String>,
}

impl Dictation {
    /// The language to recognise: the chosen one, else the editing language when the model
    /// knows it, else [`AUTO`].
    pub fn language_for(&self, editing_lang: Option<&str>) -> String {
        if let Some(l) = &self.language {
            return l.clone();
        }
        editing_lang.and_then(model_language).unwrap_or("en").to_string()
    }
}

/// One step of typing a dictated phrase.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Text(String),
    NewParagraph,
}

/// Is `c` a character a new phrase doesn't need a space after (start of text, a space, an
/// opening bracket or quote)?
fn no_space_after(c: Option<char>) -> bool {
    match c {
        None => true,
        Some(c) => c.is_whitespace() || "([{\"'“‘«¿¡/-–—".contains(c) || c == PAGE_BREAK || c == COLUMN_BREAK,
    }
}

/// Drop the recogniser's non-speech tags: anything in square brackets (`[BLANK_AUDIO]`,
/// `[Music]`) and a phrase that is only a parenthesised or starred sound (`(coughs)`, `*sigh*`)
/// or music notes.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '[' => depth = depth.saturating_add(1),
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let t = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let sound = |open: char, close: char| t.starts_with(open) && t.ends_with(close) && t.chars().filter(|&c| c == open).count() == 1;
    if sound('(', ')') || (t.len() > 1 && sound('*', '*')) || t.chars().all(|c| "♪♫♬ ".contains(c)) {
        return String::new();
    }
    t
}

/// The words of a spoken command, lower case without punctuation (`"New paragraph."` → `new
/// paragraph`).
fn word_key(w: &str) -> String {
    w.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A dictated phrase as the steps to type it, given the characters before and after the caret
/// (`None` at the start or end of a paragraph). `english` turns the spoken commands "new line"
/// and "new paragraph" into paragraph breaks.
pub fn pieces(phrase: &str, before: Option<char>, after: Option<char>, english: bool) -> Vec<Piece> {
    let phrase = clean(phrase);
    let words: Vec<&str> = phrase.split(' ').filter(|w| !w.is_empty()).collect();
    let mut out = Vec::new();
    let mut prev = before;
    let mut chunk: Vec<&str> = Vec::new();
    let flush = |chunk: &mut Vec<&str>, out: &mut Vec<Piece>, prev: &mut Option<char>| {
        if chunk.is_empty() {
            return;
        }
        let mut text = chunk.join(" ");
        chunk.clear();
        // "Dear Sam, new paragraph" leaves "Dear Sam," with its comma; a phrase that was only
        // punctuation before the command leaves nothing.
        if text.chars().all(|c| !c.is_alphanumeric()) && matches!(out.last(), Some(Piece::NewParagraph)) {
            return;
        }
        let starts_with_punct = text.starts_with(|c: char| ",.;:!?)]}…%".contains(c));
        if !no_space_after(*prev) && !starts_with_punct {
            text.insert(0, ' ');
        }
        *prev = text.chars().last();
        out.push(Piece::Text(text));
    };
    let mut i = 0;
    while i < words.len() {
        let pair = english
            && words.get(i).is_some_and(|w| word_key(w) == "new")
            && words.get(i + 1).is_some_and(|w| matches!(word_key(w).as_str(), "line" | "paragraph"));
        if pair {
            // Punctuation the recogniser put before the command ("Thanks. New paragraph") stays
            // with the text before it; the command's own ("paragraph.") goes.
            flush(&mut chunk, &mut out, &mut prev);
            out.push(Piece::NewParagraph);
            prev = None;
            i += 2;
            continue;
        }
        if let Some(w) = words.get(i) {
            // Punctuation left over from a command ("New paragraph. Thanks" → ". Thanks").
            if !(matches!(out.last(), Some(Piece::NewParagraph)) && chunk.is_empty() && w.chars().all(|c| !c.is_alphanumeric())) {
                chunk.push(w);
            }
        }
        i += 1;
    }
    flush(&mut chunk, &mut out, &mut prev);
    // Dictated in front of a word: keep a space between them.
    if let Some(Piece::Text(t)) = out.last_mut()
        && after.is_some_and(|c| c.is_alphanumeric() || "(\"“‘«".contains(c))
        && !t.ends_with(char::is_whitespace)
    {
        t.push(' ');
    }
    // A new paragraph's text starts with a capital, as after Enter.
    let mut after_break = before.is_none();
    for p in &mut out {
        match p {
            Piece::NewParagraph => after_break = true,
            Piece::Text(t) if after_break => {
                let trimmed = t.trim_start();
                let lead = t.len() - trimmed.len();
                if let Some(first) = trimmed.chars().next()
                    && first.is_lowercase()
                {
                    let upper: String = first.to_uppercase().collect();
                    t.replace_range(lead..lead + first.len_utf8(), &upper);
                }
                after_break = false;
            }
            Piece::Text(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Piece {
        Piece::Text(s.into())
    }

    #[test]
    fn spaces_join_phrases() {
        assert_eq!(pieces(" Hello there.", None, None, true), vec![text("Hello there.")]);
        assert_eq!(pieces(" How are you?", Some('.'), None, true), vec![text(" How are you?")]);
        assert_eq!(pieces("world", Some(' '), None, true), vec![text("world")]);
        assert_eq!(pieces(", and then", Some('d'), None, true), vec![text(", and then")]);
        assert_eq!(pieces("quoted", Some('“'), None, true), vec![text("quoted")]);
    }

    #[test]
    fn a_space_before_the_next_word() {
        assert_eq!(pieces("Hello there.", None, Some('T'), true), vec![text("Hello there. ")]);
        assert_eq!(pieces("Hello", Some(' '), Some(' '), true), vec![text("Hello")]);
        assert_eq!(pieces("Hello", Some(' '), Some('.'), true), vec![text("Hello")]);
    }

    #[test]
    fn non_speech_tags_are_dropped() {
        assert_eq!(pieces("[BLANK_AUDIO]", Some('x'), None, true), vec![]);
        assert_eq!(pieces(" (coughs)", Some('x'), None, true), vec![]);
        assert_eq!(pieces("♪ ♪", Some('x'), None, true), vec![]);
        assert_eq!(pieces("Yes [Music] indeed", None, None, true), vec![text("Yes indeed")]);
        // A real parenthesis in a sentence stays.
        assert_eq!(pieces("It works (mostly) fine", None, None, true), vec![text("It works (mostly) fine")]);
    }

    #[test]
    fn spoken_new_paragraph() {
        assert_eq!(
            pieces("Dear Sam, new paragraph. Thank you for the letter.", None, None, true),
            vec![text("Dear Sam,"), Piece::NewParagraph, text("Thank you for the letter.")]
        );
        assert_eq!(pieces("New line.", Some('.'), None, true), vec![Piece::NewParagraph]);
        assert_eq!(pieces("end. New paragraph, next one", Some('x'), None, true), vec![text(" end."), Piece::NewParagraph, text("Next one")]);
        // Only in English.
        assert_eq!(pieces("new line", None, None, false), vec![text("New line")]);
    }

    #[test]
    fn paragraph_start_is_capitalised() {
        assert_eq!(pieces("and so", None, None, true), vec![text("And so")]);
        assert_eq!(pieces("and so", Some('a'), None, true), vec![text(" and so")]);
    }

    #[test]
    fn language_tags_map_to_the_model() {
        assert_eq!(model_language("pt-BR"), Some("pt"));
        assert_eq!(model_language("nb-NO"), Some("no"));
        assert_eq!(model_language("sv"), Some("sv"));
        assert_eq!(model_language("x-klingon"), None);
        let d = Dictation::default();
        assert_eq!(d.language_for(Some("de-DE")), "de");
        assert_eq!(d.language_for(None), "en");
    }
}
