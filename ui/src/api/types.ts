// The API's types, generated from the Rust structs by `PINAKES_WRITE_TYPES=1 cargo test --lib api::types`.
// Never edit by hand.

/**
 * The account as the Settings page shows it (never the password).
 */
export type Account = { user: string, 
/**
 * Whether the password is still `1234`.
 */
default_password: boolean, 
/**
 * Where the account comes from: `default`, `environment` or `console` (changed there).
 */
source: string, };

export type AccountChange = { current_password: string, user: string, password: string, };

/**
 * A tenant's own switch and the sites it trusts.
 */
export type AcquisitionSettings = { enabled: boolean, sources: Array<AllowedSource>, };

export type AcquisitionStatus = { 
/**
 * Whether the deployment has acquisition on.
 */
available: boolean, settings: AcquisitionSettings, 
/**
 * The last reading of the trusted sites.
 */
last_crawl: CrawlReport | null, budget: Budget, last_run: RunReport | null, 
/**
 * Pages runs turned back, newest first.
 */
refused: Array<Note>, 
/**
 * Proposals a person rejected, newest first.
 */
rejected: Array<Note>, };

/**
 * One trusted site: a domain (its subdomains included), the license its content comes under,
 * whether the tenant may keep copies of it, and addresses to start reading it from.
 */
export type AllowedSource = { domain: string, license?: string | null, 
/**
 * Without it, a fetched page's text is kept only until it is filed; then only its address
 * and what the librarian wrote from it stay.
 */
copy: boolean, 
/**
 * Pages of the site to start from, besides what its sitemaps list; their origins are
 * where its `robots.txt` and sitemaps are read. Without them, `https://<domain>/`.
 */
urls?: Array<string>, };

/**
 * A page anchored to a walked node, or to the start node's type.
 */
export type AnchoredPage = { page: string, kind: Kind, title: string, summary: string, 
/**
 * The node or node type it is anchored to.
 */
anchored_to: string, read: boolean, };

export type Applies = "now" | "restart";

export type Approved = { proposal: string, status: ProposalStatus, 
/**
 * The job that applied it, finished.
 */
job: JobInfo, };

export type AuditEntry = { at: string, actor: string, action: string, detail: JsonValue, };

export type AuditLog = { entries: Array<AuditEntry>, };

export type Budget = { day: string, daily_tokens: number, used: number, };

export type Caller = { tenant: string, user: string | null, groups: Array<string>, };

/**
 * A page found outside and proposed as a source, held in the review queue.
 */
export type Candidate = { gap: Gap, url: string, title: string, 
/**
 * Its headings, as the librarian saw them when choosing.
 */
snippet: string, 
/**
 * Why the librarian chose it.
 */
why: string, 
/**
 * What the screening says the page holds.
 */
summary: string, license?: string | null, copy: boolean, fetched_at: string, };

/**
 * Text on a page that a finding is about, as the page has it.
 */
export type Claim = { page: string, text: string, };

/**
 * What the sign-in page and the console need to know about the service.
 */
export type ConsoleSession = { deployment: Deployment, identity: IdentityMode, 
/**
 * The headers carrying the credential, the user, the groups, an insert's audience and
 * the tenant looked at.
 */
key_header: string, user_header: string, group_header: string, audience_header: string, tenant_header: string, 
/**
 * The password is still the default one: the sign-in page says what it is.
 */
default_password: boolean, };

export type Converters = { 
/**
 * File extensions the service converts itself (besides Markdown and text).
 */
builtin: Array<string>, 
/**
 * Whether MarkItDown is set up, and whether it answers (with its version).
 */
markitdown: boolean, markitdown_version: string | null, markitdown_problem: string | null, };

export type Coverage = "strong" | "weak";

/**
 * What a crawl did.
 */
export type CrawlReport = { at: string, 
/**
 * The allowlist it read, so a changed one is read again at once.
 */
sites: Array<string>, 
/**
 * Pages read and kept.
 */
pages: number, 
/**
 * Pages whose reading failed (the site, the converter, a type that can't be read).
 */
failed: number, 
/**
 * Addresses `robots.txt` asked not to read.
 */
disallowed: number, 
/**
 * Pages no longer found, taken out of the index.
 */
removed: number, };

export type CreateTenantRequest = { tenant_id: string, };

export type CreateTenantResponse = { tenant_id: string, 
/**
 * Shown once; only its hash is stored.
 */
api_key: string, };

/**
 * `POST /ingest/graph/data`.
 */
export type DataRequest = { space: string, 
/**
 * When given, must be the space's schema.
 */
schema: string | null, nodes: Array<NodeIn>, edges: Array<EdgeIn>, };

export type DataResponse = { space: string, nodes_written: number, edges_written: number, };

export type DecisionIn = { topic: string, decision: string, context: string | null, outcome: string | null, entities: Array<string>, date: string | null, 
/**
 * The session the agent searched and read with while deciding.
 */
session: string | null, };

/**
 * Who the service is set up for (design, "Personal deployments").
 */
export type Deployment = "multi_tenant" | "personal";

/**
 * Which edges a walk follows, seen from the node it stands on.
 */
export type Direction = "out" | "in" | "both";

/**
 * How a job knew which document it was filing.
 */
export type DocumentMatch = "given" | "matched" | "new";

/**
 * One call that changed the draft.
 */
export type DraftNote = { at: string, 
/**
 * The calling user's folder, when there was one.
 */
by?: string | null, reason: string, changes: number, };

export type DraftRequest = { space: string, 
/**
 * Changes to merge into the draft; none to only see it. Read as [`Change`]s, fields that
 * are `null` left out: an agent following the tool's one schema for every kind of change
 * may send each field, `null` where it doesn't apply.
 */
changes: Array<JsonValue>, 
/**
 * Why, for the person who reviews it; required with changes.
 */
reason: string | null, };

/**
 * A space's pending draft: its proposal (none when nothing is pending) and its net change.
 */
export type DraftView = { proposal: string | null, space: string, 
/**
 * Nodes it adds or changes, as they would be.
 */
put_nodes: Array<NodeRecord>, 
/**
 * Stored nodes it removes, with their edges.
 */
remove_nodes: Array<NodeKey>, add_edges: Array<EdgeRecord>, 
/**
 * Stored edges it removes.
 */
remove_edges: Array<EdgeRecord>, 
/**
 * The latest calls that changed it.
 */
notes: Array<DraftNote>, 
/**
 * Every user who changed it (folders), kept however many notes are: deleting one of them
 * deletes the draft.
 */
authors?: Array<string>, };

export type Edge = { space: string, from: string, from_name: string, type: string, to: string, to_name: string, 
/**
 * The key the relation joins on, from the schema.
 */
via: string | null, 
/**
 * Why a search returned it; absent in walks and paths.
 */
match: EdgeMatch | null, };

/**
 * An edge as a caller writes it: ends as `type/key` inside the request's space.
 */
export type EdgeIn = { from: string, type: string, to: string, };

/**
 * How an edge reached the relations slot.
 */
export type EdgeMatch = "path" | "relation" | "node_type" | "neighbor" | "anchor";

/**
 * One edge of a space, as stored. Both ends are in the same space.
 */
export type EdgeRecord = { space: string, type: string, from_type: string, from_key: string, to_type: string, to_key: string, };

/**
 * One edit. `create`, `update` and `supersede` come from the librarian's plan; `replace` is a
 * hand edit through `PUT /pages`.
 */
export type Edit = { "op": "create", page: string, title: string, summary: string, aliases: Array<string>, keywords: Array<string>, about: Array<string>, body: string, } | { "op": "update", page: string, heading?: string | null, text?: string | null, summary?: string | null, add_aliases?: Array<string>, add_keywords?: Array<string>, add_about?: Array<string>, } | { "op": "supersede", page: string, claim: string, replacement?: string | null, reason: string, } | { "op": "replace", page: string, 
/**
 * The version (`Page::version`) the editor read, or none for a new page; the edit is
 * refused when the page is no longer at it.
 */
base?: string | null, front_matter: FrontMatter, body: string, };

export type Embedding = { 
/**
 * Whether a model is configured at all.
 */
configured: boolean, enabled: boolean, status: EmbeddingState, };

/**
 * What the embedding model is doing, as the service reports it (`GET /admin/embedding`).
 */
export type EmbeddingState = { "state": "off" } | { "state": "downloading", file: string, done_bytes: number, total_bytes: number, 
/**
 * Files already in place, of all it needs.
 */
files_done: number, files: number, } | { "state": "loading" } | { "state": "embedding", 
/**
 * The tenant being worked on (from 1), of the tenants open.
 */
tenant: number, tenants: number, 
/**
 * Texts (page heads and sections) embedded, of those the tenant lacks.
 */
done: number, total: number, } | { "state": "on" } | { "state": "failed", error: string, };

export type EmbeddingStatus = { 
/**
 * `[embedding] model`; `None` when none is configured and embeddings can't be turned on.
 */
model: string | null, 
/**
 * Whether it is turned on (it may still be loading).
 */
enabled: boolean, 
/**
 * The step it is at.
 */
status: EmbeddingState, 
/**
 * Its files on disk.
 */
files: ModelFiles, 
/**
 * Where a first download comes from: Hugging Face, or the mirror in `HF_ENDPOINT`.
 */
download_from: string, 
/**
 * About how much a first download takes.
 */
download_bytes: number, };

/**
 * An environment variable the service or its compose file reads.
 */
export type EnvVar = { name: string, description: string, set: boolean, 
/**
 * Its value, for variables that aren't secret and are set.
 */
value: string | null, secret: boolean, 
/**
 * Where it is set: the service's environment, or the compose file's `.env` too.
 */
set_in: string, };

export type Environment = { variables: Array<EnvVar>, };

export type ExperienceIn = { 
/**
 * The task type, such as "partial refund after shipment".
 */
task: string, 
/**
 * What happened.
 */
outcome: string, details: string | null, 
/**
 * Only read by the librarian, from the saved record.
 */
succeeded: boolean | null, entities: Array<string>, date: string | null, 
/**
 * The session the agent searched and read with during the task (see [`read_pages`]).
 */
session: string | null, };

export type ExpertiseIn = { title: string, content: string, 
/**
 * The id the service gave this document when it was first filed (`result.document` of
 * that job): this upload is its new version. Without it, the librarian recognizes the
 * document itself, or files it as a new one.
 */
document: string | null, source_url: string | null, entities: Array<string>, date: string | null, };

/**
 * One field as the console shows it.
 */
export type FieldView = { path: string, section: string, description: string, description_zh: string, applies: Applies, advanced: boolean, default: JsonValue, 
/**
 * Whether the console may change it (see [`editable`]).
 */
editable: boolean, 
/**
 * The value the running service uses.
 */
value: JsonValue, 
/**
 * The value in the file, when it differs from the running one (a restart applies it).
 */
saved: JsonValue | null, } & ({ "kind": "text" } | { "kind": "optional_text" } | { "kind": "integer" } | { "kind": "boolean" } | { "kind": "choice", "options": Array<string> } | { "kind": "path" });

/**
 * One finding, as `GET /lint` lists it.
 */
export type Finding = { kind: FindingKind, 
/**
 * The pages it is about, as `/read` takes them.
 */
pages: Array<string>, detail: string, 
/**
 * What a broken link or anchor names, the record, or the keyword.
 */
target?: string | null, claims: Array<Claim>, 
/**
 * For a partial name: the nodes it named.
 */
nodes: Array<string>, 
/**
 * For a weak keyword or a partial name: in how many searches.
 */
count?: number | null, 
/**
 * The held proposal with a fix, if one was proposed.
 */
proposal?: string | null, };

export type FindingKind = "broken_link" | "bad_anchor" | "unanchored" | "missing_source" | "orphan_record" | "unfinished_reading" | "frequent_miss" | "partial_name" | "contradiction" | "duplicate" | "stale";

/**
 * The front matter fields the service reads and writes. Fields it doesn't model (such as a
 * graph space's `nodes` and `relations`) are kept in `extra` and written back unchanged.
 */
export type FrontMatter = { kind?: Kind | null, title?: string | null, aliases?: Array<string>, 
/**
 * Search terms in both languages, written by the librarian.
 */
keywords?: Array<string>, summary?: string | null, 
/**
 * Anchors: the entities and pages this page is about.
 */
about?: Array<string>, sources?: Array<string>, updated?: string | null, };

/**
 * A gap: a weak keyword or a stale claim from lint.
 */
export type Gap = { 
/**
 * Whose searches or pages it came from (the finding's audience): usually the audience
 * its answer is filed for, but a personal deployment's user's gaps are filed for the
 * tenant. Deleting that owner removes what acquisition kept of the gap.
 */
owner: string, 
/**
 * `frequent_miss` or `stale`.
 */
kind: FindingKind, 
/**
 * The keyword, or the stale claim's page.
 */
target: string, detail: string, 
/**
 * For a stale claim: the claim.
 */
claim?: string | null, };

/**
 * What approving a draft applied.
 */
export type GraphApplied = { space: string, nodes_put: number, nodes_removed: number, edges_added: number, edges_removed: number, };

/**
 * The pending draft of one space: its net change from the stored graph.
 */
export type GraphDraft = { space: string, 
/**
 * Nodes it adds or changes, as they would be.
 */
put_nodes: Array<NodeRecord>, 
/**
 * Stored nodes it removes, with their edges.
 */
remove_nodes: Array<NodeKey>, add_edges: Array<EdgeRecord>, 
/**
 * Stored edges it removes.
 */
remove_edges: Array<EdgeRecord>, 
/**
 * The latest calls that changed it.
 */
notes: Array<DraftNote>, 
/**
 * Every user who changed it (folders), kept however many notes are: deleting one of them
 * deletes the draft.
 */
authors?: Array<string>, };

export type GraphRequest = { node: string, depth: number | null, direction: Direction, 
/**
 * Relation types to follow (`placed_by` or `commerce:placed_by`); all when empty.
 */
relations: Array<string>, max_nodes: number | null, session: string | null, };

export type GraphResponse = { node: NodeOut, 
/**
 * Every node reached, the start first.
 */
nodes: Array<NodeOut>, edges: Array<Edge>, pages: Array<AnchoredPage>, 
/**
 * The walk stopped at `max_nodes` with more to see.
 */
truncated: boolean, };

export type GraphRoute = { graph: string, path: PathRoute | null, };

/**
 * An edit held for review, with why the verifier objected.
 */
export type HeldEdit = { edit: Edit, reason: string, };

export type HistoryResponse = { page: string, 
/**
 * Each text the page had, oldest first, while the search traces that name its versions
 * are kept (`[trace] retention_days`).
 */
versions: Array<PageVersion>, };

export type Hit = { page: string, kind: Kind, title: string, match: MatchedBy, matched_keywords: Array<string>, summary: string, via: Via | null, lines: Array<HitLine>, about: Array<string>, 
/**
 * The raw records the page cites, as `/read` ids (`sources/<name>`).
 */
sources: Array<string>, 
/**
 * The page was written from a page found outside (`crate::acquisition`): lower trust.
 */
external?: boolean, route: Route, read: boolean, };

export type HitLine = { heading: string | null, text: string, };

export type IdentityMode = "api_key" | "jwt";

export type Intent = "relation" | "procedure" | "history";

/**
 * A job as `/jobs/{id}` shows it.
 */
export type JobInfo = { job: string, kind: string, status: JobState, created_at: string, finished_at: string | null, result: JobResult | null, error: string | null, 
/**
 * In the answer to `/ingest/expertise` only: the document id the caller sent. Without
 * one, the id is in `result.document` once the job is done.
 */
document: string | null, };

export type JobList = { jobs: Array<JobInfo>, };

/**
 * What a job did.
 */
export type JobResult = { pages_created: Array<string>, pages_updated: Array<string>, 
/**
 * Edits refused by the checks code runs, with the reason.
 */
rejected: Array<Rejected>, 
/**
 * The proposal holding the edits the verifier didn't accept, if any.
 */
proposal: string | null, 
/**
 * The raw record the job filed, as `/read` takes it.
 */
source: string | null, 
/**
 * For a document (`/ingest/expertise`): its id, issued by the service the first time the
 * document is filed. Send it as `document` with the next upload of the same document so
 * that upload is filed as its new version.
 */
document: string | null, 
/**
 * How the job knew the document: `given` (the caller sent the id), `matched` (the
 * librarian recognized a document filed before, from its wiki pages) or `new` (a new id).
 */
document_match: DocumentMatch | null, 
/**
 * The document's version this upload became, from 1.
 */
version: number | null, 
/**
 * For a lint run: what it found and proposed (the findings are at `GET /lint`).
 */
lint: LintRun | null, 
/**
 * For an acquisition run: what it searched for and proposed.
 */
acquisition: RunReport | null, 
/**
 * For an approved graph draft: what it applied.
 */
graph: GraphApplied | null, };

export type JobState = "queued" | "running" | "done" | "failed";

export type Jobs = { queued: number, running: number, };

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;

/**
 * Where the librarian's key comes from.
 */
export type KeySource = "file" | "environment" | "entered";

export type Kind = "experience" | "decision" | "expertise" | "graph" | "preference";

export type Librarian = { on: boolean, model: string | null, off_because: string | null, };

export type LinkSummary = { page: string, title: string, summary: string, };

export type LintReport = { 
/**
 * The last run; absent before the first.
 */
run: LintRun | null, findings: Array<Finding>, };

/**
 * What a lint run did.
 */
export type LintRun = { at: string, findings: number, 
/**
 * Pages the model reviewed this run.
 */
pages_reviewed: number, 
/**
 * Changed pages left for the next run.
 */
pages_waiting: number, 
/**
 * How many proposals this run held with fixes (each finding names its own).
 */
proposals: number, };

export type ListResponse = { pages: Array<PageSummary>, };

/**
 * Which LLM API the librarian calls (design, "The librarian: how new content is filed").
 */
export type LlmProvider = "anthropic" | "openai_compatible";

export type Matched = { gap: string, 
/**
 * Pages of the index that matched it (at most the few shown to the librarian).
 */
pages: number, 
/**
 * Whose gap it was.
 */
owner: string, };

export type MatchedBy = "alias" | "keyword" | "meaning" | "anchor";

export type Members = { users?: Array<string>, groups?: Array<string>, };

/**
 * How the memories and knowledge channels rank pages. Names are matched in every mode.
 */
export type Mode = "keyword" | "semantic" | "both" | "auto";

/**
 * A change to the model: a field left out stays as it is; `null` unsets it. The key is
 * write-only: sent to set it, `null` or `""` to remove it from the file.
 */
export type ModelChange = { provider?: LlmProvider, base_url?: string | null, model?: string | null, key_env?: string | null, api_key?: string | null, };

export type ModelFiles = { dir: string, 
/**
 * Bytes on disk; 0 when they were never downloaded.
 */
bytes: number, };

/**
 * The result of one tiny call to a model.
 */
export type ModelTest = { ok: boolean, 
/**
 * How long the call took.
 */
ms: number, error: string | null, };

/**
 * The model as the console shows it; the key is never in it, only where it comes from.
 */
export type ModelView = { 
/**
 * The model file.
 */
file: string, provider: LlmProvider, base_url: string | null, model: string | null, 
/**
 * The variable read for the key when the file holds none.
 */
key_env: string, 
/**
 * Where the key comes from, or `None` when there is none.
 */
key: KeySource | null, 
/**
 * Whether the librarian is on, and why not when it is off.
 */
librarian: boolean, off_because: string | null, presets: Array<Preset>, };

/**
 * A node as a caller writes it; its space comes from the request.
 */
export type NodeIn = { type: string, key: string, 
/**
 * Defaults to the key.
 */
name: string | null, aliases: Array<string>, 
/**
 * Values of properties its type declares; a node written again replaces them all.
 */
properties: { [key in string]: JsonValue }, };

/**
 * Which node: its space, type and key.
 */
export type NodeKey = { space: string, type: string, key: string, };

export type NodeList = { nodes: Array<NodeOut>, 
/**
 * Nodes matching, over all pages.
 */
total: number, };

export type NodeOut = { id: string, space: string, type: string, key: string, name: string, aliases: Array<string>, 
/**
 * Values of the properties its type declares.
 */
properties?: { [key in string]: JsonValue }, 
/**
 * Edges from the node the walk started at.
 */
depth: number, };

/**
 * One node of a space, as stored.
 */
export type NodeRecord = { space: string, type: string, key: string, name: string, aliases: Array<string>, 
/**
 * Values of the properties its type declares.
 */
properties?: { [key in string]: JsonValue }, };

export type NodeTypeDef = { 
/**
 * The field that identifies a node of this type, such as `order_id`.
 */
key: string, en?: string | null, zh?: string | null, aliases?: Array<string>, 
/**
 * What a node of this type may hold besides its name and aliases, as the schema's author
 * declares it (decided with the user 2026-10-10).
 */
properties?: { [key in string]: PropertyDef }, };

export type Note = { at: string, url: string, reason: string, 
/**
 * Whose gap it was for (`tenant`, or a group or user folder): who may see it, and what
 * deleting that owner removes.
 */
owner: string, };

export type PageResponse = { page: string, kind: Kind | null, 
/**
 * The page's version; a `PUT` sends it back.
 */
version: string, front_matter: FrontMatter, body: string, 
/**
 * The body as safe HTML, when asked for (`?html=true`): raw HTML shown as text, only
 * web and mail links, no images, wiki links as console routes (`wiki::render`).
 */
html: string | null, 
/**
 * Written from a page found outside (it cites an external source): lower trust.
 */
external: boolean, };

export type PageSlot = { coverage: Coverage, hits: Array<Hit>, };

/**
 * What `/pages` lists for each page: enough to pick one, not its body.
 */
export type PageSummary = { 
/**
 * The page id, as search hits and `/read` name it.
 */
page: string, kind: Kind | null, title: string | null, summary: string | null, updated: string | null, };

/**
 * One text a page had, as [`History::page_versions`] lists it.
 */
export type PageVersion = { 
/**
 * The stored wiki version that holds it.
 */
version: string, 
/**
 * When the wiki came to that text (a page changed back lists its earlier text again,
 * with the time it came back).
 */
saved_at: string, };

export type PathRequest = { from: string, to: string, relations: Array<string>, session: string | null, };

export type PathResponse = { 
/**
 * A chain of at most `MAX_PATH_HOPS` edges exists. Nodes of two spaces are never joined.
 */
found: boolean, 
/**
 * The path's nodes from `from` to `to`, with their distance from `from`.
 */
nodes: Array<NodeOut>, 
/**
 * The edges in path order; each keeps its own direction.
 */
edges: Array<Edge>, };

export type PathRoute = { from: string, to: string, };

export type PreferenceIn = { statement: string, 
/**
 * An observed habit rather than something the user said. Only read by the librarian,
 * from the saved record.
 */
observed: boolean, entities: Array<string>, date: string | null, };

/**
 * A common endpoint, to fill the form with.
 */
export type Preset = { name: string, provider: LlmProvider, base_url: string | null, model: string, key_env: string, };

/**
 * A property a node type declares: the kind of value it holds, and what it means.
 */
export type PropertyDef = { type: PropertyType, en?: string | null, zh?: string | null, };

/**
 * The kinds of property values: text, a number, a date (`YYYY-MM-DD`) or true/false.
 */
export type PropertyType = "text" | "number" | "date" | "bool";

/**
 * Edits held for review (design, "Filling gaps from outside, with approval": one queue for
 * held edits and, from M9, external candidates). Approving one applies its edits through the
 * same code checks; the verifier is not asked again.
 */
export type Proposal = { proposal: string, status: ProposalStatus, created_at: string, kind: Kind, 
/**
 * `tenant`, or the group or user folder the pages are for.
 */
audience: string, 
/**
 * The raw record the edits were planned from.
 */
source?: string | null, edits: Array<HeldEdit>, 
/**
 * The writer's user and group folders, for checking anchors on approval.
 */
user?: string | null, groups?: Array<string>, 
/**
 * A page found outside, proposed as a source to file (`crate::acquisition`); its record
 * is `source`, and it has no edits.
 */
candidate?: Candidate | null, 
/**
 * Why a person rejected it.
 */
reason?: string | null, 
/**
 * An agent's draft of changes to a graph space (`graph::draft`); it has no edits.
 */
graph?: GraphDraft | null, };

export type ProposalList = { proposals: Array<Proposal>, };

export type ProposalStatus = "held" | "approved" | "rejected";

export type PutPage = { 
/**
 * The `version` `GET` returned; none (or absent) for a new page.
 */
version: string | null, 
/**
 * What `GET` also returns, so its answer can be sent back as it is; ignored (the path
 * says which page, and code writes `kind`).
 */
page: JsonValue | null, kind: JsonValue | null, front_matter: FrontMatter, body: string, };

export type ReadItem = { id: string, kind: Kind | null, title: string | null, summary: string | null, heading: string | null, text: string | null, 
/**
 * Already read in this session: the text is left out.
 */
already_read?: boolean, 
/**
 * A page written from a page found outside, or such a source itself: lower trust.
 */
external?: boolean, links: Array<LinkSummary>, 
/**
 * `not_found`, `unreadable` (the page's file can't be parsed) or `heading_not_found`;
 * the other single-value fields are then absent and `links` is empty (lists are always
 * present; design, "Response formats").
 */
error: string | null, };

export type ReadRequest = { ids: Array<string>, heading: string | null, with_links: boolean, session: string | null, };

export type ReadResponse = { pages: Array<ReadItem>, };

export type RejectResponse = { proposal: string, status: ProposalStatus, };

export type Rejected = { page: string, reason: string, };

export type RelationDef = { from: string, to: string, 
/**
 * The key field the relation joins on, such as `customer_id`.
 */
via?: string | null, en?: string | null, zh?: string | null, aliases?: Array<string>, };

export type RelationSlot = { coverage: Coverage, 
/**
 * Relation types the request names, as `schema:relation`.
 */
relation_types: Array<string>, edges: Array<Edge>, 
/**
 * Where to go next: a walk from the first named node, and the path between the first two
 * named nodes of one space.
 */
route: GraphRoute | null, };

export type Route = { read: string, heading: string | null, };

/**
 * What a run did.
 */
export type RunReport = { at: string, 
/**
 * Gaps found in the last lint run, and how many were taken this run.
 */
gaps: number, gaps_taken: number, 
/**
 * Each gap matched against the trusted sites' pages, with how many pages matched.
 */
matched: Array<Matched>, 
/**
 * Gaps nothing was proposed for before choosing, with why.
 */
skipped: Array<Skipped>, proposals: Array<string>, 
/**
 * Pages turned back this run (the reasons are at `GET /acquisition`).
 */
refused: number, tokens: number, budget_left: number, 
/**
 * Why the run stopped early, if it did.
 */
stopped?: string | null, };

/**
 * A graph schema: node types and relation types. Its stored copy is authoritative.
 */
export type SchemaDef = { schema: string, title?: string | null, summary?: string | null, aliases?: Array<string>, nodes: { [key in string]: NodeTypeDef }, relations: { [key in string]: RelationDef }, };

export type SchemaList = { schemas: Array<SchemaOut>, };

export type SchemaOut = { 
/**
 * Its description in the wiki (`graph/<schema>`), written by the librarian.
 */
page: string, 
/**
 * Spaces under it the caller may see.
 */
spaces: Array<string>, schema: string, title?: string | null, summary?: string | null, aliases?: Array<string>, nodes: { [key in string]: NodeTypeDef }, relations: { [key in string]: RelationDef }, };

export type SchemaResponse = { schema: string, 
/**
 * The schema's wiki description.
 */
page: string, 
/**
 * A new schema, rather than a change to an existing one.
 */
created: boolean, };

export type SearchRequest = { query: string, keywords: Array<string>, 
/**
 * `[search] default_mode` when absent.
 */
mode?: Mode | null, entities: Array<string>, intent: Intent | null, slots: SlotRequest, 
/**
 * Follow the top memories to the knowledge pages they are anchored to.
 */
hop: boolean, session: string | null, };

export type SearchResponse = { 
/**
 * The mode the search ran in: the request's, or the default.
 */
mode: Mode, entities: Array<string>, intent: Intent | null, memories: PageSlot, knowledge: PageSlot, relations: RelationSlot, unmatched_keywords: Array<string>, 
/**
 * Set when the search ran by keywords alone though its mode would rank by meaning too
 * (`auto` while semantic search is off), saying so.
 */
note: string | null, };

export type SectionView = { name: string, description: string, description_zh: string, };

export type SetEmbedding = { enabled: boolean, };

export type SettingsChange = { 
/**
 * `section.key` → the new value; `null` puts a field back to its default.
 */
changes: { [key in string]: JsonValue }, };

export type SettingsView = { 
/**
 * The config file, when the service was started from one; without one nothing can be
 * written.
 */
file: string | null, sections: Array<SectionView>, fields: Array<FieldView>, 
/**
 * Whether a saved change waits for a restart.
 */
restart_needed: boolean, };

export type SignIn = { user: string, password: string, };

export type SignedIn = { 
/**
 * The session token: the console's credential, sent in the key header.
 */
token: string, account: Account, };

export type Skipped = { gap: string, reason: string, owner: string, };

export type SlotRequest = { memories: number | null, knowledge: number | null, relations: number | null, };

export type SpaceList = { spaces: Array<SpaceOut>, };

export type SpaceOut = { space: string, schema: string, name: string | null, aliases?: Array<string>, visibility: Visibility, 
/**
 * Whether the caller created it.
 */
mine: boolean, 
/**
 * Who else may see a restricted space.
 */
members: Members | null, 
/**
 * Nodes per type.
 */
nodes: { [key in string]: number }, edges: number, };

/**
 * What a caller sends to create a space. The owner is never in the body: it is the caller.
 */
export type SpaceRequest = { space: string, schema: string, 
/**
 * Display name, such as 跨境站.
 */
name: string | null, aliases: Array<string>, visibility: Visibility, members: Members, };

export type SpaceResponse = { space: string, schema: string, name: string | null, visibility: Visibility, };

export type Status = { 
/**
 * The code answering: crate version and commit.
 */
service: string, deployment: Deployment, identity: IdentityMode, caller: Caller, librarian: Librarian, embedding: Embedding, converters: Converters, 
/**
 * `sqlite` or `neo4j`.
 */
graph: string, jobs: Jobs, 
/**
 * Proposals waiting for review that the caller may see.
 */
proposals: number, 
/**
 * Whether the service listens only on this machine.
 */
local_only: boolean, 
/**
 * Whether the developer chat (`/dev/chat`, `PINAKES_DEV=1`) is on.
 */
dev: boolean, };

export type TenantAudit = { entries: Array<TenantAuditEntry>, };

export type TenantAuditEntry = { at: string, actor: string, action: string, detail: string, };

export type TenantList = { tenants: Array<TenantOut>, };

export type TenantOut = { tenant_id: string, 
/**
 * Its folder under `<data>/tenants`.
 */
dir: string, 
/**
 * Whether its search snapshot is in memory now.
 */
in_memory: boolean, };

/**
 * How a hit got into its slot other than by matching the request itself.
 */
export type Via = "memory";

/**
 * Who can see a graph space.
 */
export type Visibility = "shared" | "private" | "restricted";
