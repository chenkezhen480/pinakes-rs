// A small hash router: `#/wiki/expertise/returns?tab=history`. The section is the first part
// of the path; the rest is the page's own.
import { flushSync } from "svelte";
import { viewTransition } from "./motion";

/** Each part of a route path decoded on its own; a broken escape is kept as it is. */
function decodePath(path: string): string {
  return path
    .split("/")
    .map((part) => {
      try {
        return decodeURIComponent(part);
      } catch {
        return part;
      }
    })
    .join("/");
}

/** Each part of a route path encoded whole (`?`, `&`, `#` and the like included); `:` kept. */
function encodePath(path: string): string {
  return path
    .split("/")
    .map((part) => encodeURIComponent(part).replaceAll("%3A", ":"))
    .join("/");
}

function parse(): { path: string; query: URLSearchParams } {
  const raw = location.hash.replace(/^#/, "") || "/";
  const [path = "/", search = ""] = raw.split("?", 2);
  return { path: decodePath(path), query: new URLSearchParams(search) };
}

class Router {
  path = $state("/");
  query = $state(new URLSearchParams());

  constructor() {
    const p = parse();
    this.path = p.path;
    this.query = p.query;
    window.addEventListener("hashchange", () => {
      const next = parse();
      this.path = next.path;
      this.query = next.query;
    });
  }

  /** The path's parts: `/wiki/expertise/returns` is `["wiki", "expertise", "returns"]`. */
  get parts(): string[] {
    return this.path.split("/").filter((p) => p !== "");
  }

  get section(): string {
    return this.parts[0] ?? "overview";
  }

  /** The path after the section. */
  get rest(): string {
    return this.parts.slice(1).join("/");
  }
}

export const router = new Router();

/** The href of a console route, query values encoded. */
export function href(path: string, query?: Record<string, string | undefined>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(query ?? {})) if (v !== undefined && v !== "") q.set(k, v);
  const s = q.toString();
  return `#${encodePath(path)}${s ? `?${s}` : ""}`;
}

export function go(path: string, query?: Record<string, string | undefined>): void {
  location.hash = href(path, query);
}

/** Changes the query of the current route without a new history entry. */
export function setQuery(query: Record<string, string | undefined>): void {
  const target = href(router.path, query);
  history.replaceState(null, "", target);
  router.query = new URLSearchParams(target.split("?")[1] ?? "");
}

/** Goes to a route as a view transition (a card opening into the reader), where the browser
 * has them; elsewhere it just goes. */
export function goWithTransition(path: string): void {
  viewTransition(() => {
    const target = href(path);
    history.pushState(null, "", target);
    router.path = path;
    router.query = new URLSearchParams();
    flushSync();
  });
}
