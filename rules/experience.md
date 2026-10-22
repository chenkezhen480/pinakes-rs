# Rules: agent experience

Agent experience is what agents learned while doing tasks, including problems users ran into.
It comes back in the memories slot.

## Page shape

- One playbook per task type, such as "partial refund after shipment". The title names the task
  type, not one incident.
- Sections: `## Steps that work` (numbered), `## Pitfalls` (each pitfall with its fix), and
  optionally `## Notes`.
- Each pitfall and each step that came from experience says how often it was seen: "Seen 3 times."
- Write only what the record says happened. A pitfall is a mistake or problem the record reports.
  Advice the record gives ("put an unsubscribe link in every email") is a step, never turned into
  a pitfall claiming it was missing or done wrong ("the email had no unsubscribe link") unless the
  record says so.
- The summary is a sentence or two an agent can act on without opening the page, in English and
  the same in Chinese.

## Merging

- If a playbook for this task type already exists among the related pages, update it. Never
  create a second page for the same task type.
- A repeat of something the page already says raises its count ("Seen 2 times" becomes
  "Seen 3 times"); it is not written again.
- A new pitfall or a better step is added to its section. A step the new record proves wrong is
  marked superseded, not deleted.
- Only create a new page when no related page covers this task type.

## Anchors

- Anchor the page to the node types, nodes and pages the task touched (`about`). When nothing
  listed is what the task was about, leave `about` empty: a wrong anchor brings an unrelated
  page into later searches, a missing one only skips a shortcut.
- Prefer node types (such as `commerce:order`) over single nodes: a playbook is about a kind of thing, not
  one order.
- Also anchor the knowledge pages (expertise, schema descriptions) the task relied on: the
  rules, limits or procedures the playbook follows. When the item lists knowledge pages the
  caller read for the task, anchor those the outcome used. A search that finds the playbook
  then brings those pages along.
