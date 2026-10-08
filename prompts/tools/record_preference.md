Records a preference the user stated ("reply to me in Chinese", "always show amounts with tax"),
so later tasks respect it. Call it when the user says how they want things, not for one-off
instructions about the current message.

- `statement`: the preference in one or two sentences, in the user's terms.
- `entities`: optional ids of graph nodes or types it concerns, as `search` returned them.

It is filed for the user, in the background.
