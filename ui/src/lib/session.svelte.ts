// Who the console is: the service's admin, by the session token signing in gave, looking at a
// tenant as a user and groups of it would see it (a personal deployment has its own tenant).
// Kept in this browser's local storage, so the next visit is signed in; it is sent in a header,
// never a cookie, so there is nothing for another site to ride on.
import type { ConsoleSession } from "../api/types";

const STORE = "pinakes.console";

type Saved = { token: string; tenant: string; user: string; groups: string };

function load(): Saved {
  try {
    const raw = localStorage.getItem(STORE);
    if (raw) {
      const v: unknown = JSON.parse(raw);
      if (v && typeof v === "object") {
        const o = v as Record<string, unknown>;
        const s = (k: string) => (typeof o[k] === "string" ? (o[k] as string) : "");
        return { token: s("token"), tenant: s("tenant"), user: s("user"), groups: s("groups") };
      }
    }
  } catch {
    // nothing kept
  }
  return { token: "", tenant: "", user: "", groups: "" };
}

class Session {
  token = $state("");
  /** The tenant looked at (multi-tenant deployments). */
  tenant = $state("");
  /** The user and groups looked at as, sent as their headers. */
  user = $state("");
  groups = $state("");
  /** About the service, from `GET /console/session`. */
  info = $state<ConsoleSession | null>(null);

  constructor() {
    this.adopt(load());
    // Another tab signed in, out, or chose whom to look at as: follow it.
    window.addEventListener("storage", (e) => {
      if (e.key === STORE || e.key === null) this.adopt(load());
    });
  }

  private adopt(s: Saved): void {
    this.token = s.token;
    this.tenant = s.tenant;
    this.user = s.user;
    this.groups = s.groups;
  }

  get signedIn(): boolean {
    return this.token !== "";
  }

  get personal(): boolean {
    return this.info?.deployment === "personal";
  }

  /** Whether a tenant has to be chosen before the tenant's pages can be shown. */
  get needsTenant(): boolean {
    return this.info !== null && !this.personal && this.tenant === "";
  }

  save(): void {
    try {
      const saved: Saved = { token: this.token, tenant: this.tenant, user: this.user, groups: this.groups };
      localStorage.setItem(STORE, JSON.stringify(saved));
    } catch {
      // kept for this page only
    }
  }

  signIn(token: string): void {
    this.token = token;
    this.save();
  }

  /** Looks at a tenant as a user and groups. */
  view(tenant: string, user: string, groups: string): void {
    this.tenant = tenant.trim();
    this.user = user.trim();
    this.groups = groups.trim();
    this.save();
  }

  /** A call made with `token` was refused: that session ended. Forgotten if it is still the
   *  one this browser holds; a newer one another tab saved is taken up instead. */
  ended(token: string): void {
    const stored = load();
    if (stored.token && stored.token !== token) {
      this.adopt(stored);
      return;
    }
    if (this.token === token) this.signOut();
  }

  /** Forgets the session (the service is told by `api.signOut`). */
  signOut(): void {
    this.token = "";
    this.save();
  }
}

export const session = new Session();
