The only way into the knowledge base. Returns three slots at once: `memories` (experience,
decisions, preferences), `knowledge` (expertise, schema descriptions) and `relations` (edges from
the business graph). Each hit has its `page` id, a `summary`, the page's `lines` that matched (up to
three, with their heading), the `matched_keywords`, its `about` anchors and a `route` saying how to
open it; `read: true` means this session already read it. Each
slot has a `coverage` of `strong` or `weak`; `unmatched_keywords` lists what found nothing.
`preferences`, when present, is the user's whole preferences page (reply language, formats, the
role or accounts they handle), sent with every search: follow it in every answer, whatever the
question is about.

Write the request well; the service does no rewriting:

- `query`: the request as one sentence with meaning, in the user's language.
- `keywords`: 2 to 6 short terms of 1 to 3 words each: names, identifiers, codes, canonical
  domain terms, and both English and Chinese forms of the key terms.
  - Good: `["partial refund", "部分退款", "order 1042"]`.
  - Bad: `["how do I refund a customer who already got the goods"]` (a sentence, not a term),
    `["the", "customer"]` (too generic).
- Abstract first-person questions into the topic they are about. "What language should replies to
  me use?" becomes keywords like `["preferences", "reply language", "偏好", "回复语言"]`;
  "我上次怎么处理退款的？" becomes `["refund", "退款", "decision", "决策"]`.
- Put ids exactly as the service writes them when you have one: `commerce:order/1042` for a node,
  `commerce:order` for a node type.

Then:

- Read the summaries and `lines` first. When they hold what was asked, answer from them: opening
  the page costs another step. Open a hit, following its `route` (`read` with the page id, `graph`
  with a node id), only for what they leave out.
- `mode` picks how pages are ranked; leave it out at first. `auto` (the usual default) ranks by
  your keywords and by meaning together, or by keywords alone when the service has meaning
  search off (the result's `note` says so). `keyword` ranks by the terms only: for exact names,
  identifiers and codes. `semantic` ranks by meaning only, which finds pages that say it in other
  words or in the other language.
- On a `weak` slot or unmatched keywords: retry with other terms (synonyms, the other language,
  the identifier), or with `mode: "semantic"`. When the service has meaning search off,
  `semantic` answers that it is unavailable: retry with other keywords.
- Pass the `session` from the result to every later call of the task.
