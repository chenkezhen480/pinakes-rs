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
  a sentence or two, in English and the same in Chinese.

## Merging

- The latest statement wins: a new statement on the same topic replaces the old line.
- A stated preference outranks an observed habit; an observed habit is marked "(observed)" and
  never replaces a stated preference.

## Anchors

- The page is anchored to its user by where it is (the user's folder): no anchor is required.
- Add an anchor only when preferences really concern something, such as the node type a role
  or an export format is about. A reply language or an answer format concerns no node or page:
  leave `about` empty rather than add an anchor just to have one.
