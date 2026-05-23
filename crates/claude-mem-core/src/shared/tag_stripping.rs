//! Privacy tag stripping (port of `utils/tag-stripping.ts`).
//!
//! `<private>content</private>` blocks are removed at the hook layer so they
//! never reach the worker or database.

use std::borrow::Cow;

const PRIVATE_OPEN: &str = "<private>";
const PRIVATE_CLOSE: &str = "</private>";

/// Remove every `<private>…</private>` block (including the tags).
pub fn strip_private_tags(input: &str) -> Cow<'_, str> {
    if !input.contains(PRIVATE_OPEN) {
        return Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open) = rest.find(PRIVATE_OPEN) {
        out.push_str(&rest[..open]);
        match rest[open + PRIVATE_OPEN.len()..].find(PRIVATE_CLOSE) {
            Some(close) => {
                rest = &rest[open + PRIVATE_OPEN.len() + close + PRIVATE_CLOSE.len()..];
            }
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tags_returns_borrowed() {
        let borrowed = strip_private_tags("plain text");
        assert!(matches!(borrowed, Cow::Borrowed(_)));
        assert_eq!(borrowed, "plain text");
    }

    #[test]
    fn strips_single_block() {
        assert_eq!(
            strip_private_tags("hello <private>secret</private> world"),
            "hello  world"
        );
    }

    #[test]
    fn strips_multiple_blocks() {
        assert_eq!(
            strip_private_tags("<private>a</private>X<private>b</private>"),
            "X"
        );
    }

    #[test]
    fn strips_unclosed_block() {
        assert_eq!(strip_private_tags("hello <private>never closed"), "hello ");
    }
}
