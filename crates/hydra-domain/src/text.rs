/// Detects controls that can make a short public identifier appear to say
/// something other than its signed byte sequence.
pub(crate) fn has_unsafe_inline_text(value: &str) -> bool {
    value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_directional_spoofing_without_rejecting_rtl_text() {
        assert!(has_unsafe_inline_text("Hydra \u{202e}ardyH"));
        assert!(has_unsafe_inline_text("Hydra\u{2066}name\u{2069}"));
        assert!(!has_unsafe_inline_text("هايدرا"));
        assert!(!has_unsafe_inline_text("Hydra 🐍"));
    }
}
