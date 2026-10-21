# Rules: user decision history

A decision record is what a user (or a group, deciding together) chose and what happened as a
result. It comes back in the memories slot, only to that user or group.

## Page shape

- One page per decision topic, such as "refund policy". The title names the topic.
- Each decision is its own section, newest last: `## <date>: <short decision>`, then the
  decision, its context, and its outcome when the record gives one. A record without an outcome
  gets no outcome line: never write a placeholder such as "outcome not known".
- The summary states the current position on the topic in a sentence or two, in English and the
  same in Chinese.

## Merging

- If a page for this topic exists among the related pages, add the new decision to it as a new
  section. Never create a second page for the same topic.
- Past entries are never overwritten or edited. A decision that reverses an earlier one says so
  in its own section ("Reverses the decision of 2026-08-01"); the summary is updated to the new
  position.
- An outcome that arrives later for an earlier decision is added as a new section too
  (`## <date>: outcome of <decision>`).

## Anchors

- Anchor the page to what the decision was about: node types, nodes or pages. At least one
  anchor is required.
- Also anchor the knowledge pages (expertise, schema descriptions) the decision rested on. When
  the item lists knowledge pages the caller read while deciding, anchor those it used.
