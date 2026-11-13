//! Token accounting for tool output.
//!
//! Every result the harness shows a model is capped twice: once per call and
//! once per turn. The estimator is deliberately dependency-free. It is not a
//! tokenizer; it is a budgeting guard rail that stays within roughly ±15% of a
//! BPE tokenizer for English prose and errs high for dense text.

/// Approximate token count. ASCII text is about four characters per token;
/// non-ASCII text is closer to one token per character, so it is billed higher.
pub fn estimate_tokens(text: &str) -> usize {
    let mut ascii = 0usize;
    let mut other = 0usize;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            other += 1;
        }
    }
    (ascii as f64 / 4.0).ceil() as usize + other
}

/// A tool result after the budget has been applied.
#[derive(Clone, Debug)]
pub struct Allotment {
    pub text: String,
    pub tokens: usize,
    pub clipped: bool,
}

/// A per-turn allowance plus the counters that make savings measurable.
#[derive(Clone, Debug)]
pub struct Budget {
    cap_tokens: usize,
    spent_tokens: usize,
    /// Characters the same questions would have cost without the harness.
    pub raw_chars: usize,
    /// Characters actually emitted to the model.
    pub compact_chars: usize,
    /// Per-call clipping events, surfaced in the run report.
    pub clips: usize,
}

impl Budget {
    pub fn new(cap_tokens: usize) -> Self {
        Self {
            cap_tokens,
            spent_tokens: 0,
            raw_chars: 0,
            compact_chars: 0,
            clips: 0,
        }
    }

    pub fn cap_tokens(&self) -> usize {
        self.cap_tokens
    }

    pub fn spent_tokens(&self) -> usize {
        self.spent_tokens
    }

    pub fn remaining_tokens(&self) -> usize {
        self.cap_tokens.saturating_sub(self.spent_tokens)
    }

    pub fn is_exhausted(&self) -> bool {
        self.remaining_tokens() < 24
    }

    /// Fit `text` into the per-call and per-turn caps.
    ///
    /// `raw_chars` is the size of the naive alternative this call replaced
    /// (for example, the full resume body). It feeds the savings report and
    /// never affects what the model sees.
    pub fn take(&mut self, text: String, call_cap_tokens: usize, raw_chars: usize) -> Allotment {
        let call_cap_chars = call_cap_tokens.max(32) * 4;
        let turn_cap_chars = self.remaining_tokens().saturating_mul(4).max(64);

        if self.is_exhausted() {
            let text =
                "!budget: tool allowance spent. Answer from the results already shown.".to_string();
            let tokens = estimate_tokens(&text);
            self.spent_tokens = self.cap_tokens;
            self.compact_chars += text.len();
            return Allotment {
                text,
                tokens,
                clipped: true,
            };
        }

        let (mut fitted, per_call_clip) = clip_chars(&text, call_cap_chars);
        let (further, turn_clip) = clip_chars(&fitted, turn_cap_chars);
        fitted = further;
        let clipped = per_call_clip || turn_clip;
        if clipped {
            self.clips += 1;
        }

        let tokens = estimate_tokens(&fitted);
        self.spent_tokens = (self.spent_tokens + tokens).min(self.cap_tokens);
        self.compact_chars += fitted.len();
        self.raw_chars += raw_chars.max(fitted.len());
        Allotment {
            text: fitted,
            tokens,
            clipped,
        }
    }

    /// Tokens avoided compared with the raw payloads the calls replaced.
    pub fn saved_tokens(&self) -> usize {
        self.raw_chars.saturating_sub(self.compact_chars) / 4
    }
}

/// Clip to a character budget on a word boundary where possible.
fn clip_chars(text: &str, max_chars: usize) -> (String, bool) {
    if text.len() <= max_chars {
        return (text.to_string(), false);
    }
    let mut end = max_chars;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let slice = &text[..end];
    let cut = slice
        .rfind(['\n', ' '])
        .filter(|pos| *pos > max_chars / 2)
        .unwrap_or(slice.len());
    let omitted = text.len().saturating_sub(cut);
    (
        format!("{}\n…(+{omitted} chars clipped)", &text[..cut]),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_ascii_and_non_ascii() {
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert!(estimate_tokens("こんにちは") >= 5);
    }

    #[test]
    fn clips_to_the_call_cap_and_counts_the_saving() {
        let mut budget = Budget::new(1_000);
        let text = "word ".repeat(1_000);
        let out = budget.take(text, 100, 20_000);
        assert!(out.clipped);
        assert!(out.text.len() <= 100 * 4 + 40, "{}", out.text.len());
        assert!(budget.saved_tokens() > 3_000);
    }

    #[test]
    fn a_spent_budget_refuses_new_work() {
        let mut budget = Budget::new(20);
        let out = budget.take("a".repeat(1_000), 200, 1_000);
        assert!(out.text.starts_with("!budget"));
        assert!(budget.is_exhausted());
    }
}
