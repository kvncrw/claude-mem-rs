//! Approximate token counting (port of `services/context/TokenCalculator.ts`).
//!
//! Uses a fixed ~4-chars-per-token estimate; the TS implementation used the
//! same heuristic.

const CHARS_PER_TOKEN: f64 = 4.0;

pub fn estimate_tokens(text: &str) -> usize {
    (text.len() as f64 / CHARS_PER_TOKEN).ceil() as usize
}

pub fn estimate_tokens_for_lines(lines: &[&str]) -> usize {
    lines.iter().map(|l| estimate_tokens(l)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn rounds_up() {
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }
}
