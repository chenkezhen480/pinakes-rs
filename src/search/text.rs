//! Text normalization shared by the index, queries and the name matcher.
//!
//! Everything is compared in normalized form: NFKC (so full-width letters and digits become
//! ASCII) and lowercase. Normalized text is then split into runs of Chinese characters and runs
//! of word characters (letters and digits of other scripts).

use unicode_normalization::UnicodeNormalization;

/// NFKC plus lowercase.
pub fn normalize(s: &str) -> String {
    s.nfkc().flat_map(char::to_lowercase).collect()
}

/// Han characters. Chinese text is segmented by jieba and by bigrams, never by spaces.
pub fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{2A6DF}')
}

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() && !is_cjk(c)
}

/// Characters that join word characters into one identifier: `order_status`, `cnc-07`, `v2.1`.
fn is_joiner(c: char) -> bool {
    matches!(c, '_' | '-' | '.')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Cjk,
    Word,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run<'a> {
    pub kind: RunKind,
    pub text: &'a str,
}

/// Splits (normalized) text into Chinese runs and word runs; everything else separates them.
/// A word run keeps inner joiners, so an identifier stays one run.
pub fn runs(s: &str) -> Vec<Run<'_>> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (start, c) = chars[i];
        let kind = if is_cjk(c) {
            RunKind::Cjk
        } else if is_word_char(c) {
            RunKind::Word
        } else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < chars.len() {
            let c = chars[j].1;
            let same = match kind {
                RunKind::Cjk => is_cjk(c),
                RunKind::Word => {
                    is_word_char(c)
                        || (is_joiner(c) && chars.get(j + 1).is_some_and(|&(_, n)| is_word_char(n)))
                }
            };
            if !same {
                break;
            }
            j += 1;
        }
        let end = chars.get(j).map_or(s.len(), |&(b, _)| b);
        out.push(Run {
            kind,
            text: &s[start..end],
        });
        i = j;
    }
    out
}

/// The parts of a word run split at joiners: `cnc-07` → `cnc`, `07`.
pub fn word_parts(run: &str) -> impl Iterator<Item = &str> {
    run.split(is_joiner).filter(|p| !p.is_empty())
}

/// Whether text contains at least one Chinese character.
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(is_cjk)
}

/// Whether every character is Chinese.
pub fn all_cjk(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_cjk)
}

/// Whether `pattern` found at `start..end` of `text` sits on word boundaries: a pattern that
/// starts or ends with a word character must not continue a longer word.
pub fn on_word_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let first = text[start..end].chars().next();
    let last = text[start..end].chars().next_back();
    let clash = |edge: Option<char>, outside: Option<char>| {
        edge.is_some_and(is_word_char) && outside.is_some_and(is_word_char)
    };
    !clash(first, before) && !clash(last, after)
}

/// Common English words that say nothing about the topic. Used only on query text the agent
/// didn't turn into keywords, never on indexed text.
const EN_STOP: &[&str] = &[
    "a", "about", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as", "at",
    "be", "been", "before", "being", "best", "but", "by", "can", "could", "did", "do", "does",
    "doing", "done", "for", "from", "get", "give", "go", "good", "had", "has", "have", "he", "her",
    "here", "him", "his", "how", "i", "if", "in", "into", "is", "it", "its", "just", "know", "let",
    "like", "make", "me", "my", "need", "new", "no", "not", "now", "of", "on", "one", "or", "our",
    "out", "please", "right", "s", "say", "she", "should", "so", "some", "tell", "than", "that",
    "the", "their", "them", "then", "there", "these", "they", "this", "those", "to", "tomorrow",
    "today", "up", "us", "use", "want", "was", "way", "we", "were", "what", "when", "where",
    "which", "who", "why", "will", "with", "would", "yes", "you", "your",
];

/// Common Chinese function words and question words.
const ZH_STOP: &[&str] = &[
    "的",
    "了",
    "吗",
    "呢",
    "吧",
    "啊",
    "呀",
    "是",
    "在",
    "有",
    "没有",
    "我",
    "你",
    "他",
    "她",
    "它",
    "我们",
    "你们",
    "他们",
    "咱们",
    "这",
    "那",
    "这个",
    "那个",
    "这些",
    "那些",
    "什么",
    "怎么",
    "怎么样",
    "怎样",
    "如何",
    "哪",
    "哪个",
    "哪些",
    "哪里",
    "为什么",
    "能",
    "会",
    "要",
    "想",
    "可以",
    "应该",
    "需要",
    "一个",
    "一下",
    "一些",
    "和",
    "与",
    "及",
    "或",
    "或者",
    "还",
    "也",
    "都",
    "就",
    "把",
    "被",
    "给",
    "让",
    "对",
    "从",
    "到",
    "于",
    "之",
    "其",
    "帮",
    "帮我",
    "请",
    "写",
    "关于",
    "一首",
    "现在",
    "今天",
    "明天",
    "昨天",
    "多少",
    "几",
    "么",
    "又",
    "再",
    "得",
    "地",
    "着",
    "过",
    "个",
    "该",
    "很",
    "最",
    "更",
    "已经",
    "之前",
    "以后",
    "时候",
    "怎么办",
    "是否",
    "是不是",
    "有没有",
    "能不能",
    "告诉",
    "知道",
    "处理",
    "情况",
];

pub fn is_stopword(word: &str) -> bool {
    if has_cjk(word) {
        ZH_STOP.contains(&word)
    } else {
        EN_STOP.contains(&word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_texts(s: &str) -> Vec<(RunKind, &str)> {
        runs(s).into_iter().map(|r| (r.kind, r.text)).collect()
    }

    #[test]
    fn normalizes_width_and_case() {
        assert_eq!(normalize("ＣＮＣ－０７ Refund"), "cnc-07 refund");
        assert_eq!(normalize("部分退款"), "部分退款");
    }

    #[test]
    fn splits_runs() {
        use RunKind::*;
        assert_eq!(
            run_texts("订单1042的order_status, cnc-07 v2.1。"),
            vec![
                (Cjk, "订单"),
                (Word, "1042"),
                (Cjk, "的"),
                (Word, "order_status"),
                (Word, "cnc-07"),
                (Word, "v2.1")
            ]
        );
        assert_eq!(
            run_texts("end. -x what's"),
            vec![(Word, "end"), (Word, "x"), (Word, "what"), (Word, "s")]
        );
        assert_eq!(word_parts("cnc-07").collect::<Vec<_>>(), vec!["cnc", "07"]);
    }

    #[test]
    fn word_boundaries() {
        let t = "ssot and sso login";
        assert!(!on_word_boundary(t, 0, 3));
        assert!(on_word_boundary(t, 9, 12));
        // Chinese patterns have no word boundaries.
        let z = "客户订单号";
        assert!(on_word_boundary(z, 6, 12));
    }
}
