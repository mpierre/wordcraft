//! Text the speech model invents rather than hears.
//!
//! Given a scrap of sound with little speech in it (a breath, the end of a word, the click of
//! Stop), Whisper writes something anyway: "you", "Thank you.", or, when it has been shown the
//! text before the caret, that text again or a polite sentence to follow it ("Thank you for your
//! time."). Typed into the document, that reads as dictation repeating itself. A phrase's text is
//! kept only when it could have been spoken in the time there was speech, isn't just the words
//! before the caret again, and isn't one of the model's fillers from under a second of sound.

/// Fastest speech counted as real: words per second of speech, with one word to spare (people
/// speak two to four words a second).
const MAX_WORDS_PER_SEC: f32 = 5.0;
/// Below this much speech, the model's usual fillers are taken for invented text.
const FILLER_SECS: f32 = 1.0;
/// What the model writes for sound with no words in it.
const FILLERS: &[&str] =
    &["you", "thank you", "thanks", "thank you very much", "thanks for watching", "thank you for watching", "bye", "okay", "so", "um", "uh", "hmm"];
/// Phrases with at least this much speech are shown the text before the caret (shorter ones
/// invent more with it than they gain).
pub const PROMPT_SECS: f32 = 1.0;

/// The words of `text`, lower case, without punctuation.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric() || *c == '\'').flat_map(char::to_lowercase).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether `text`, recognised from `voiced_secs` of speech after `context` (the text before the
/// caret), was heard rather than invented.
pub fn heard(text: &str, context: &str, voiced_secs: f32) -> bool {
    let said = words(text);
    if said.is_empty() {
        return false;
    }
    if said.len() as f32 > 1.0 + voiced_secs.max(0.0) * MAX_WORDS_PER_SEC {
        return false;
    }
    if voiced_secs < FILLER_SECS && FILLERS.contains(&said.join(" ").as_str()) {
        return false;
    }
    // The words just before the caret again (two words or more: one word may well be said twice).
    let before = words(context);
    if said.len() >= 2 && before.windows(said.len()).any(|w| w == said.as_slice()) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::heard;

    const CONTEXT: &str = "Dear committee, thank you for reading my letter. I would like to propose a new schedule for the spring term.";

    #[test]
    fn real_phrases_are_kept() {
        assert!(heard(" Dear committee,", "", 0.9));
        assert!(heard(" I would like to propose a new schedule for the spring term.", "Thank you.", 3.2));
        assert!(heard(" Yes.", CONTEXT, 0.3));
        // A filler said at length is speech.
        assert!(heard(" Thank you.", "The end.", 1.4));
        // One word said again is fine.
        assert!(heard(" term", CONTEXT, 0.5));
    }

    #[test]
    fn invented_text_from_a_scrap_of_sound_is_dropped() {
        // What Whisper base wrote for the last 0.3 s of the sentence, shown the sentence.
        assert!(!heard(" Thank you for your time.", CONTEXT, 0.3));
        // Room noise and a click.
        assert!(!heard(" you", CONTEXT, 0.25));
        assert!(!heard(" Thank you.", CONTEXT, 0.3));
        assert!(!heard("", CONTEXT, 2.0));
        assert!(!heard(" ...", CONTEXT, 2.0));
    }

    #[test]
    fn the_text_before_the_caret_again_is_dropped() {
        assert!(!heard(" Spring term.", CONTEXT, 0.6));
        assert!(!heard(" I would like to propose a new schedule for the spring term.", CONTEXT, 3.0));
        assert!(heard(" A new schedule for the autumn term.", CONTEXT, 2.0));
    }
}
