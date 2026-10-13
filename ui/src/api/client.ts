// The console's calls to the service's HTTP API, with the signed-in admin's session. The
// types are generated from the Rust structs (`types.ts`); request bodies are built here with the
// service's own defaults, so a field left out means what it means to the service.
import type * as T from "./types";
import { session } from "../lib/session.svelte";

/** An API error: its status, code and the service's own message (written to be read). */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

type Query = Record<string, string | number | boolean | undefined | null>;

interface Options {
  body?: unknown;
  form?: FormData;
  /** Who an insert is for: `tenant`, `user` or `group:<id>`. */
  audience?: string;
  query?: Query;
  /** No credentials at all (signing in). */
  anonymous?: boolean;
}

function headers(o: Options): Headers {
  const h = new Headers();
  const info = session.info;
  const keyHeader = info?.key_header ?? "Authorization";
  if (!o.anonymous && session.token) {
    const token = session.token;
    h.set(keyHeader, keyHeader.toLowerCase() === "authorization" ? `Bearer ${token}` : token);
    // The tenant, user and groups looked at as.
    if (session.tenant && !session.personal)
      h.set(info?.tenant_header ?? "X-Pinakes-Tenant", encodeURIComponent(session.tenant));
    if (session.user) h.set(info?.user_header ?? "X-User-Id", session.user);
    if (session.groups) h.set(info?.group_header ?? "X-User-Groups", session.groups);
  }
  if (o.audience) h.set(info?.audience_header ?? "X-Audience", o.audience);
  if (o.body !== undefined) h.set("Content-Type", "application/json");
  return h;
}

/** The caller's credential headers, for the few calls made outside `api` (a streamed chat). */
export function callerHeaders(): Headers {
  return headers({ body: {} });
}

function url(path: string, query?: Query): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(query ?? {})) {
    if (v !== undefined && v !== null && v !== "") q.set(k, String(v));
  }
  const s = q.toString();
  return s ? `${path}?${s}` : path;
}

async function call<R>(method: string, path: string, o: Options = {}): Promise<R> {
  // A header that can't carry what was typed (a user id outside Latin-1) is said as such, not
  // as an unreachable service.
  let h: Headers;
  try {
    h = headers(o);
  } catch {
    throw new ApiError(400, "bad_request", "a user or group id can't be sent in a header: use letters, digits and punctuation without accents");
  }
  const sentWith = session.token;
  let resp: Response;
  try {
    resp = await fetch(url(path, o.query), {
      method,
      headers: h,
      body: o.form ?? (o.body === undefined ? undefined : JSON.stringify(o.body)),
      credentials: "omit",
    });
  } catch {
    throw new ApiError(0, "unreachable", "");
  }
  if (resp.status === 204) {
    const none: unknown = null;
    return none as R;
  }
  const text = await resp.text();
  let data: unknown = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = text;
    }
  }
  if (!resp.ok) {
    // A session that ended (signed out elsewhere, the password changed, expired): sign in again.
    if (resp.status === 401 && !o.anonymous && sentWith) session.ended(sentWith);
    let code = "error";
    let message = typeof data === "string" ? data : "";
    if (data && typeof data === "object" && "error" in data) {
      const e = (data as { error: { code?: string; message?: string } }).error;
      code = e.code ?? code;
      message = e.message ?? message;
    }
    throw new ApiError(resp.status, code, message);
  }
  return data as R;
}

const enc = (path: string) => path.split("/").map(encodeURIComponent).join("/");

export const api = {
  // Signing in and the overview
  consoleSession: () => call<T.ConsoleSession>("GET", "/console/session", { anonymous: true }),
  signIn: (user: string, password: string) => {
    const body: T.SignIn = { user, password };
    return call<T.SignedIn>("POST", "/console/sign-in", { anonymous: true, body });
  },
  signOut: () => call<null>("POST", "/console/sign-out"),
  account: () => call<T.Account>("GET", "/console/account"),
  changeAccount: (body: T.AccountChange) => call<T.SignedIn>("PUT", "/console/account", { body }),
  status: () => call<T.Status>("GET", "/status"),

  // The wiki
  pages: (kind?: T.Kind) => call<T.ListResponse>("GET", "/pages", { query: { kind } }),
  page: (path: string) => call<T.PageResponse>("GET", `/pages/${enc(path)}`, { query: { html: true } }),
  putPage: (path: string, body: T.PutPage) => call<T.JobInfo>("PUT", `/pages/${enc(path)}`, { body }),
  history: (path: string) => call<T.HistoryResponse>("GET", `/history/${enc(path)}`),
  historyVersion: (path: string, version: string) =>
    call<T.PageResponse>("GET", `/history/${enc(path)}`, { query: { version } }),

  // Searching and reading
  search: (query: string, more: Partial<T.SearchRequest> = {}) => {
    const body: T.SearchRequest = {
      query,
      keywords: [],
      entities: [],
      intent: null,
      slots: { memories: null, knowledge: null, relations: null },
      hop: true,
      session: null,
      ...more,
    };
    return call<T.SearchResponse>("POST", "/search", { body });
  },
  read: (ids: string[], more: Partial<T.ReadRequest> = {}) => {
    const body: T.ReadRequest = { ids, heading: null, with_links: true, session: null, ...more };
    return call<T.ReadResponse>("POST", "/read", { body });
  },

  // The graph
  graph: (node: string, more: Partial<T.GraphRequest> = {}) => {
    const body: T.GraphRequest = {
      node,
      depth: null,
      direction: "both",
      relations: [],
      max_nodes: null,
      session: null,
      ...more,
    };
    return call<T.GraphResponse>("POST", "/graph", { body });
  },
  graphPath: (from: string, to: string) => {
    const body: T.PathRequest = { from, to, relations: [], session: null };
    return call<T.PathResponse>("POST", "/graph/path", { body });
  },
  schemas: () => call<T.SchemaList>("GET", "/graph/schemas"),
  spaces: () => call<T.SpaceList>("GET", "/graph/spaces"),
  nodes: (space: string, query: { type?: string; q?: string; offset?: number; limit?: number }) =>
    call<T.NodeList>("GET", "/graph/nodes", { query: { space, ...query } }),
  putSchema: (def: T.SchemaDef) => call<T.SchemaResponse>("POST", "/ingest/graph/schema", { body: def }),
  putSpace: (req: T.SpaceRequest) => call<T.SpaceResponse>("POST", "/ingest/graph/space", { body: req }),
  deleteSpace: (space: string) => call<T.SpaceDeleted>("DELETE", `/graph/spaces/${encodeURIComponent(space)}`),
  putSpaceAccess: (space: string, req: T.SpaceAccess) =>
    call<T.SpaceResponse>("PUT", `/graph/spaces/${encodeURIComponent(space)}`, { body: req }),
  putData: (req: T.DataRequest) => call<T.DataResponse>("POST", "/ingest/graph/data", { body: req }),
  draft: (space: string) => call<T.DraftView>("GET", "/graph/draft", { query: { space } }),

  // Adding knowledge
  experience: (body: T.ExperienceIn, audience?: string) =>
    call<T.JobInfo>("POST", "/ingest/experience", { body, audience }),
  decision: (body: T.DecisionIn, audience?: string) =>
    call<T.JobInfo>("POST", "/ingest/decision", { body, audience }),
  preference: (body: T.PreferenceIn, audience?: string) =>
    call<T.JobInfo>("POST", "/ingest/preference", { body, audience }),
  expertise: (body: T.ExpertiseIn, audience?: string) =>
    call<T.JobInfo>("POST", "/ingest/expertise", { body, audience }),
  upload: (form: FormData, audience?: string) =>
    call<T.JobInfo>("POST", "/ingest/expertise", { form, audience }),

  // Jobs and review
  jobs: () => call<T.JobList>("GET", "/jobs"),
  job: (id: string) => call<T.JobInfo>("GET", `/jobs/${encodeURIComponent(id)}`),
  proposals: () => call<T.ProposalList>("GET", "/proposals"),
  approve: (id: string) =>
    call<T.Approved>("POST", `/proposals/${encodeURIComponent(id)}/approve`),
  reject: (id: string, reason: string) =>
    call<T.RejectResponse>("POST", `/proposals/${encodeURIComponent(id)}/reject`, {
      body: { reason },
    }),

  // Lint and acquisition
  lint: () => call<T.LintReport>("GET", "/lint"),
  runLint: () => call<T.JobInfo>("POST", "/lint"),
  acquisition: () => call<T.AcquisitionStatus>("GET", "/acquisition"),
  putAcquisition: (tenant: string, settings: T.AcquisitionSettings) =>
    call<T.AcquisitionSettings>("PUT", `/admin/tenants/${encodeURIComponent(tenant)}/acquisition`, {
      body: settings,
    }),

  // Administration
  tenants: () => call<T.TenantList>("GET", "/admin/tenants"),
  createTenant: (tenant_id: string) => {
    const body: T.CreateTenantRequest = { tenant_id };
    return call<T.CreateTenantResponse>("POST", "/admin/tenants", { body });
  },
  deleteTenant: (id: string) =>
    call<null>("DELETE", `/admin/tenants/${encodeURIComponent(id)}`),
  deleteGroup: (tenant: string, group: string) =>
    call<unknown>(
      "DELETE",
      `/admin/tenants/${encodeURIComponent(tenant)}/groups/${encodeURIComponent(group)}`,
      {},
    ),
  deleteUser: (user: string) => call<unknown>("DELETE", `/users/${encodeURIComponent(user)}`),
  tenantAudit: (id: string) =>
    call<T.TenantAudit>("GET", `/admin/tenants/${encodeURIComponent(id)}/audit`),
  audit: () => call<T.AuditLog>("GET", "/admin/audit"),
  settings: () => call<T.SettingsView>("GET", "/admin/settings"),
  putSettings: (changes: Record<string, T.JsonValue>) => {
    const body: T.SettingsChange = { changes };
    return call<T.SettingsView>("PUT", "/admin/settings", { body });
  },
  environment: () => call<T.Environment>("GET", "/admin/environment"),
  model: () => call<T.ModelView>("GET", "/admin/model"),
  putModel: (change: T.ModelChange) =>
    call<T.ModelView>("PUT", "/admin/model", { body: change }),
  testModel: (change: T.ModelChange) =>
    call<T.ModelTest>("POST", "/admin/model/test", { body: change }),
  embedding: () => call<T.EmbeddingStatus>("GET", "/admin/embedding"),
  setEmbedding: (enabled: boolean) => {
    const body: T.SetEmbedding = { enabled };
    return call<T.EmbeddingStatus>("PUT", "/admin/embedding", { body });
  },
  deleteModelFiles: () =>
    call<T.EmbeddingStatus>("DELETE", "/admin/embedding/files"),
};
