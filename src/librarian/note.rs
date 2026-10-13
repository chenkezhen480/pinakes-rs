//! A note an agent recorded (`/ingest/note`, MCP `record_decision_or_lesson`; design,
//! "Recording unprompted"): the agent only judges that something is worth keeping, and the
//! librarian splits it into the decisions it holds, filed for the user, and the lessons, filed
//! for the team. What it can't tell apart stays a decision, so a misreading keeps text with the
//! user rather than showing it to everyone.

use serde::Deserialize;

use super::{ATTEMPTS, Librarian, object, parse_json, prompts, string};
use crate::kinds::Kind;

/// The most items one note is split into.
pub const MAX_ITEMS: usize = 4;

/// One item of a note, to be filed as a record of its kind.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NoteItem {
    pub kind: NoteKind,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    Decision,
    Lesson,
}

impl NoteKind {
    /// The kind of record the item is filed as.
    pub fn kind(self) -> Kind {
        match self {
            NoteKind::Decision => Kind::Decision,
            NoteKind::Lesson => Kind::Experience,
        }
    }
}

#[derive(Deserialize)]
struct Split {
    items: Vec<NoteItem>,
}

/// The reply: the items, each a decision or a lesson.
pub fn note_schema() -> serde_json::Value {
    let item = object(&[
        (
            "kind",
            serde_json::json!({ "type": "string", "enum": ["decision", "lesson"] }),
        ),
        ("title", string()),
        ("text", string()),
    ]);
    object(&[(
        "items",
        serde_json::json!({ "type": "array", "items": item }),
    )])
}

impl Librarian {
    /// The items a note holds. A reply without usable items, after every try, leaves the whole
    /// note as one decision: the user's, as the agent wrote it.
    #[tracing::instrument(name = "librarian_note", skip_all)]
    pub async fn split_note(&self, topic: &str, note: &str) -> anyhow::Result<Vec<NoteItem>> {
        let prompt = self.prompt(prompts::NOTE, format!("## Note\n\n{note}\n"), note_schema());
        for _ in 0..ATTEMPTS {
            let reply = self.llm.complete(&prompt).await?;
            match parse_json::<Split>(&reply) {
                Ok(split) => {
                    let items: Vec<NoteItem> = split
                        .items
                        .into_iter()
                        .filter(|i| !i.text.trim().is_empty())
                        .map(|mut i| {
                            if i.title.trim().is_empty() {
                                i.title = topic.to_owned();
                            }
                            i
                        })
                        .take(MAX_ITEMS)
                        .collect();
                    if !items.is_empty() {
                        return Ok(items);
                    }
                    tracing::info!("the note was split into nothing");
                }
                Err(e) => tracing::info!(error = %e, "the note's split could not be read"),
            }
        }
        Ok(vec![NoteItem {
            kind: NoteKind::Decision,
            title: topic.to_owned(),
            text: note.to_owned(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lesson_is_filed_as_experience_and_a_decision_as_a_decision() {
        assert_eq!(NoteKind::Lesson.kind(), Kind::Experience);
        assert_eq!(NoteKind::Decision.kind(), Kind::Decision);
        let reply = r#"{"items":[{"kind":"lesson","title":"t","text":"x"}]}"#;
        let split: Split = parse_json(reply).unwrap();
        assert_eq!(split.items[0].kind, NoteKind::Lesson);
        assert!(
            note_schema()["properties"]["items"]["items"]["properties"]["kind"]["enum"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("decision"))
        );
    }
}
