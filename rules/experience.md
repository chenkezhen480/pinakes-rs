# Rules: agent experience

Agent experience is what agents learned while doing tasks, including problems users ran into.
It comes back in the memories slot.

## Page shape

- One playbook per task type, such as "partial refund after shipment". The title names the task
  type, not one incident.
- Sections: `## Steps that work` (numbered), `## Pitfalls` (each pitfall with its fix), and
  optionally `## Notes`.
- Each pitfall and each step that came from experience says how often it was seen: "Seen 3 times."
- The summary is 1–3 sentences an agent can act on without opening the page.

## Merging

- If a playbook for this task type already exists among the related pages, update it. Never
  create a second page for the same task type.
- A repeat of something the page already says raises its count ("Seen 2 times" becomes
  "Seen 3 times"); it is not written again.
- A new pitfall or a better step is added to its section. A step the new record proves wrong is
  marked superseded, not deleted.
- Only create a new page when no related page covers this task type.

## Anchors

- Anchor the page to the node types, nodes and pages the task touched (`about`). At least one
  anchor is required.
- Prefer node types (`schema:type`) over single nodes: a playbook is about a kind of thing, not
  one order.
