# Configuration reference

Generated from `src/settings.rs` (`PINAKES_WRITE_REFERENCE=1 cargo test --lib settings`); do not edit by hand. Each field is set in `config.toml` (or the file `PINAKES_CONFIG` names) and shown, explained and edited on the console's Settings page. Secrets are never in this file: they are environment variables (the admin key, the Neo4j password) or in the model file.

## [server]

Where the service listens and how much it computes at once.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `host` | text | `"127.0.0.1"` | after a restart | The address the service listens on. 127.0.0.1 keeps it on this machine; 0.0.0.0 opens it to the network (use TLS in front of it then). |
| `port` | a whole number, 0 or more | `8080` | after a restart | The port it listens on. |
| `threads` | a whole number, 0 or more | `0` | after a restart | Threads the service computes on (searches, indexing, embedding). 0 means 4, or fewer on a machine with fewer CPUs. |

## [data]

Where the service keeps its data.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `dir` | text | `"./data"` | after a restart | The data folder: tenants, models, traces. Relative to where the service is started. |

## [identity]

Who callers are: API keys or JWTs, and the headers that carry users, groups and audiences.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `mode` | one of api_key, jwt | `"api_key"` | after a restart | How callers prove who they are: a tenant's API key with user and group headers, or a JWT signed by the host. |
| `deployment` | one of multi_tenant, personal | `"multi_tenant"` | after a restart | multi_tenant serves many tenants. personal serves one person: the default tenant is created on first start and a request without a user header acts as the default user. |
| `default_tenant` | text | `"000000"` | after a restart | Personal deployments: the tenant created on first start; its key is written to <data>/personal-api-key.txt. |
| `default_user` | text | `"admin"` | after a restart | Personal deployments: the user a request without the user header acts as. An ordinary user, without admin rights. |
| `key_header` | text | `"Authorization"` | after a restart | The header carrying the API key. For Authorization, a "Bearer " prefix is expected. |
| `user_header` | text | `"X-User-Id"` | after a restart | API key mode: the header naming the end user. |
| `group_header` | text | `"X-User-Groups"` | after a restart | API key mode: the header listing the caller's groups, comma-separated. |
| `audience_header` | text | `"X-Audience"` | after a restart | The header naming who an insert is for: tenant, user or group:<id>. |
| `admin_key_env` | text | `"PINAKES_ADMIN_KEY"` | after a restart | The environment variable holding the service admin key, for scripts and host code calling the admin routes; unset or empty, only the console's signed-in admin reaches them. |
| `public_key_file` | text | unset | after a restart | JWT mode: the host's public key (PEM), relative to the config file. |
| `algorithm` | one of RS256, RS384, RS512, PS256, PS384, PS512, ES256, ES384, EdDSA | `"RS256"` | after a restart | JWT mode: the algorithm tokens are signed with. |
| `tenant_claim` | text | `"tenant_id"` | after a restart | JWT mode: the claim holding the tenant id. |
| `user_claim` | text | `"sub"` | after a restart | JWT mode: the claim holding the user id. |
| `groups_claim` | text | `"groups"` | after a restart | JWT mode: the claim holding the group ids (a list). |
| `issuer` | text, or null to unset it | unset | after a restart | JWT mode: when set, a token's iss must be this. |
| `audience` | text, or null to unset it | unset | after a restart | JWT mode: when set, a token's aud must name this; when unset, a token with an aud is refused. |

## [store]

Each tenant's SQLite file.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `file_name` | text | `"pinakes.sqlite"` | after a restart | The name of each tenant's SQLite file. |

## [librarian]

The librarian, the one LLM the service calls (writes only). Which model and its key are in the model file.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `model_file` | text | `"model.conf"` | after a restart | The model file (provider, model, key), relative to the config file. Edit it on the Model page. |
| `max_tokens` | a whole number, 0 or more | `16000` | after a restart | The most tokens one reply of the model may use. |
| `timeout_secs` | a whole number, 0 or more | `300` | after a restart | How long one model call may take, in seconds. |
| `related_pages` | a whole number, 0 or more | `8` | after a restart | How many related pages the librarian is shown when it files something (5 to 10 works best). |
| `read_window_tokens` | a whole number, 0 or more | `20000` | after a restart | About how much of a document one reading step sees, in tokens; it grows when a concept runs past it. |

## [converter]

The optional MarkItDown converter, for PDF and the old binary Office formats. The service reads Markdown, text, HTML, Word, spreadsheets and PowerPoint itself.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `url` | text, or null to unset it | unset | after a restart | Where the MarkItDown converter answers, such as http://127.0.0.1:8090. Unset, PDF, .doc and .ppt files are refused with what to do. |
| `timeout_secs` | a whole number, 0 or more | `120` | after a restart | How long converting one file may take, in seconds, by the service or by MarkItDown. |

## [search]

How searches rank and how many hits each slot holds.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `slots.memories` | a whole number, 0 or more | `3` | after a restart | Hits in the memories slot when a search doesn't say (at most 20). |
| `slots.knowledge` | a whole number, 0 or more | `3` | after a restart | Hits in the knowledge slot when a search doesn't say (at most 20). |
| `slots.relations` | a whole number, 0 or more | `5` | after a restart | Hits in the relations slot when a search doesn't say (at most 20). |
| `session_idle_minutes` | a whole number, 0 or more | `30` | after a restart | A search session (which pages an agent has read) ends after this many idle minutes. |
| `default_mode` | one of keyword, semantic, both, auto | `"keyword"` | after a restart | The mode of a search that doesn't say. semantic and both need embeddings on; auto ranks by meaning only where keywords come back weak. |
| `hop.top_memories` | a whole number, 0 or more | `3` | after a restart | How many of the top memories bring the knowledge pages they are anchored to into the knowledge slot. |
| `ablation.pin_names` | true or false | `true` | after a restart | Experiment switch: titles and aliases named in a query pin their pages to the top. Leave on. (advanced) |
| `ablation.words` | true or false | `true` | after a restart | Experiment switch: search the words of each field (jieba, stemming). Leave on. (advanced) |
| `ablation.bigrams` | true or false | `true` | after a restart | Experiment switch: search the character pairs of each field. Leave on. (advanced) |
| `vector_graph_rows` | a whole number, 0 or more | `100000` | after a restart | From this many page vectors on, a tenant keeps a graph index over them for meaning search (0: never). |

## [trace]

Search traces, kept for replays.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `enabled` | true or false | `true` | after a restart | Whether searches are traced to <tenant>/logs, for replays. Wiki versions are kept only while it is on. |
| `retention_days` | a whole number, 0 or more | `30` | after a restart | Trace files older than this many days are removed; 0 keeps them. |

## [embedding]

Meaning-based search: a local embedding model, off by default.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `enabled` | true or false | `false` | at once | Whether meaning-based search is on. Turning it on downloads the model (about 640 MB, once) and embeds every page once, without a restart; turning it off keeps the stored vectors. |
| `model` | text, or null to unset it | unset | after a restart | The embedding model: jina-embeddings-v2-base-zh, the one the service runs. Unset, embeddings can't be turned on. |

## [graph]

Where graph data is kept, and the limits on walks.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `backend` | one of sqlite, neo4j | `"sqlite"` | after a restart | Where graph data is kept: graph.sqlite in each tenant's folder, or a Neo4j server. A tenant's data is moved with pinakes-migrate-graph, with the service stopped. |
| `max_depth` | a whole number, 0 or more | `3` | after a restart | The deepest graph walk a caller may ask for. |
| `max_nodes` | a whole number, 0 or more | `100` | after a restart | The most nodes a graph walk returns. |

## [neo4j]

The Neo4j server, used only with graph.backend = "neo4j".

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `uri` | text | `"bolt://localhost:7687"` | after a restart | The Neo4j server's address. |
| `user` | text | `"neo4j"` | after a restart | The Neo4j user. |
| `password_env` | text | `"NEO4J_PASSWORD"` | after a restart | The environment variable holding the Neo4j password (never in this file). |
| `database` | text, or null to unset it | unset | after a restart | The Neo4j database; the server's default when unset. |

## [acquisition]

Filling gaps from the sites a tenant trusts, with an admin's approval.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `enabled` | true or false | `false` | after a restart | Whether acquisition may run at all; each tenant turns it on too, with the sites it trusts. |
| `daily_token_budget` | a whole number, 0 or more | `200000` | after a restart | Tokens acquisition's own model calls may use per tenant and day. |
| `crawl_pages_per_site` | a whole number, 0 or more | `500` | after a restart | The most pages read from one trusted site. |
| `crawl_every_days` | a whole number, 0 or more | `7` | after a restart | Days before a tenant's sites are read again (at once when its list changes). |
| `crawl_delay_ms` | a whole number, 0 or more | `1000` | after a restart | The pause between two requests to a site, in milliseconds. |
| `allow_private_hosts` | true or false | `false` | after a restart | Lets acquisition fetch from this machine and private networks. For tests only: never on in use. (advanced) |

## [cache]

Tenants kept in memory.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `idle_minutes` | a whole number, 0 or more | `30` | after a restart | A tenant unused this long gives up its in-memory copies; 0 keeps them. |

## [jobs]

The per-tenant write queue.

| Field | Kind | Default | Applies | What it does |
| --- | --- | --- | --- | --- |
| `keep_finished_minutes` | a whole number, 0 or more | `60` | after a restart | Finished jobs stay visible this long. |
