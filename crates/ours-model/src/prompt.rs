/// Collects the parts of a system prompt so the parts that rarely change
/// come first. Endpoints cache by prefix, so a stable prefix is a cache hit.
#[derive(Debug, Clone, Default)]
pub struct PromptBuilder {
    stable: Vec<String>,
    dynamic: Vec<String>,
}

const SEPARATOR: &str = "\n\n";

impl PromptBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a part that is the same from call to call, such as the persona
    /// or the charter. Stable parts keep the order they were added in.
    pub fn stable(&mut self, part: &str) -> &mut Self {
        push_part(&mut self.stable, part);
        self
    }

    /// Adds a part that changes per call. It goes after every stable part.
    pub fn dynamic(&mut self, part: &str) -> &mut Self {
        push_part(&mut self.dynamic, part);
        self
    }

    /// Joins the parts with blank lines, stable first.
    pub fn build(&self) -> BuiltPrompt {
        let mut text = self.stable.join(SEPARATOR);
        if !self.stable.is_empty() && !self.dynamic.is_empty() {
            text.push_str(SEPARATOR);
        }
        let dynamic_offset = text.len();
        text.push_str(&self.dynamic.join(SEPARATOR));
        BuiltPrompt {
            text,
            dynamic_offset,
        }
    }
}

fn push_part(parts: &mut Vec<String>, part: &str) {
    let part = part.trim();
    if !part.is_empty() {
        parts.push(part.to_owned());
    }
}

/// An assembled system prompt and where its dynamic half begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltPrompt {
    text: String,
    dynamic_offset: usize,
}

impl BuiltPrompt {
    /// The whole prompt.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn into_string(self) -> String {
        self.text
    }

    /// Byte offset of the first dynamic part. A cache breakpoint belongs
    /// here. It is the length of the text when there is no dynamic part.
    pub fn dynamic_offset(&self) -> usize {
        self.dynamic_offset
    }

    /// The stable parts, without the blank line that follows them.
    pub fn stable_prefix(&self) -> &str {
        self.text[..self.dynamic_offset].trim_end()
    }

    /// The dynamic parts.
    pub fn dynamic_suffix(&self) -> &str {
        &self.text[self.dynamic_offset..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_parts_come_first_in_insertion_order() {
        let mut builder = PromptBuilder::new();
        builder
            .dynamic("last 30 messages")
            .stable("persona")
            .dynamic("speaker is a lead")
            .stable("charter");
        let built = builder.build();
        assert_eq!(
            built.as_str(),
            "persona\n\ncharter\n\nlast 30 messages\n\nspeaker is a lead"
        );
    }

    #[test]
    fn reports_where_the_dynamic_part_starts() {
        let mut builder = PromptBuilder::new();
        builder.stable("persona").stable("charter").dynamic("now");
        let built = builder.build();
        assert_eq!(built.dynamic_offset(), "persona\n\ncharter\n\n".len());
        assert_eq!(&built.as_str()[built.dynamic_offset()..], "now");
        assert_eq!(built.stable_prefix(), "persona\n\ncharter");
        assert_eq!(built.dynamic_suffix(), "now");
    }

    #[test]
    fn offset_counts_bytes_not_characters() {
        let mut builder = PromptBuilder::new();
        builder.stable("caf\u{e9} \u{1f600}").dynamic("x");
        let built = builder.build();
        assert_eq!(built.dynamic_offset(), "caf\u{e9} \u{1f600}\n\n".len());
        assert_eq!(built.dynamic_suffix(), "x");
    }

    #[test]
    fn only_stable_parts_put_the_offset_at_the_end() {
        let mut builder = PromptBuilder::new();
        builder.stable("persona");
        let built = builder.build();
        assert_eq!(built.as_str(), "persona");
        assert_eq!(built.dynamic_offset(), built.as_str().len());
        assert_eq!(built.stable_prefix(), "persona");
        assert_eq!(built.dynamic_suffix(), "");
    }

    #[test]
    fn only_dynamic_parts_put_the_offset_at_the_start() {
        let mut builder = PromptBuilder::new();
        builder.dynamic("now");
        let built = builder.build();
        assert_eq!(built.dynamic_offset(), 0);
        assert_eq!(built.stable_prefix(), "");
        assert_eq!(built.dynamic_suffix(), "now");
    }

    #[test]
    fn trims_parts_and_skips_empty_ones() {
        let mut builder = PromptBuilder::new();
        builder
            .stable("  persona\n")
            .stable("   ")
            .dynamic("\nnow  ")
            .dynamic("");
        assert_eq!(builder.build().as_str(), "persona\n\nnow");
    }

    #[test]
    fn empty_builder_builds_an_empty_prompt() {
        let built = PromptBuilder::new().build();
        assert_eq!(built.as_str(), "");
        assert_eq!(built.dynamic_offset(), 0);
    }
}
