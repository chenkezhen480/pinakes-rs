# Rules: graph schema descriptions

A schema description explains a graph schema in words, in English and Chinese. Code owns its
structure: node types, relation types, their ends, keys and `via`. The librarian writes only
what the types mean.

## What to write

- `title`: a short name for the schema, such as "Commerce graph".
- `summary`: 1–2 sentences on what the schema models.
- `aliases`: other names of the schema in both languages.
- For each node type and relation type: `en` and `zh`, one short phrase each saying what it is,
  and a few `aliases` people use for it (both languages; for a relation, the verbs and phrases
  people use for it, such as "placed", "ordered by", 下单).
- `body`: Markdown with `## Node types` and `## Relation types`, one line per type.

## Rules

- Describe only the types the definition has. Never invent a type, a relation, a key or an end.
- Keep descriptions the host already gave; fill in what is missing.
- Never describe individual nodes, spaces or data.
