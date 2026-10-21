# Rules: vertical expertise

Expertise is domain knowledge from documents and manuals: concepts, rules, procedures and
entities, written as linked wiki pages. It comes back in the knowledge slot.

## Page shape

- One page per concept or entity, as in a classic wiki. A long document becomes several pages,
  or updates to several existing ones.
- Start with a short definition, then sections with headings. Link related pages by their
  ids, `[[<folder>/<name>]]` such as `[[expertise/cross-border-returns]]`, and only to pages
  that exist.
- The summary defines the concept in one short sentence in English and the same in Chinese.
- Aliases hold the other names of the concept in English and Chinese; keywords hold the terms
  people search for.

## Merging

- Integrate new material into the existing page for that concept instead of creating a
  duplicate.
- When the new document contradicts a page, do not silently pick one: keep both claims and add
  a line `> Contradiction: <source A> says …; <source B> says …` under the claim.
- A claim the new document clearly replaces (a newer version of a rule, with a date) is marked
  superseded, not deleted.

## Anchors

- Anchor pages to the node types they describe when a graph schema has them. Anchors are
  optional for expertise.
