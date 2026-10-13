# Rules: user preferences

Preferences are what a user stated about how they want to be served, standing facts about them
that shape answers (the role, region, site or accounts they now handle), or habits observed in
their requests. Each user has one short profile page. It comes back in the memories slot, only
to that user.

## Page shape

- One page per user: `users/<user>/preferences`. Title: "<name>'s preferences" or "Preferences".
- A short list, one preference per line: `- <topic>: <preference>`, such as
  `- Reply language: Chinese` or `- Role: handles the EU enterprise accounts`.
- Keep it short: at most about 20 lines. The summary lists the most important preferences in
  1–2 sentences.

## Merging

- The latest statement wins: a new statement on the same topic replaces the old line.
- A stated preference outranks an observed habit; an observed habit is marked "(observed)" and
  never replaces a stated preference.

## Anchors

- Anchor the page to what the preferences concern most, such as a node type the user works
  with. At least one anchor is required.
