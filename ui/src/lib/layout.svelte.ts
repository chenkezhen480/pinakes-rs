// How the console is laid out: the Library's hall (the default where WebGL is drawn and the
// window is wide enough) or the list of drawers. Chosen with a switch and remembered in this
// browser; a hall that fails to draw falls back to the list for this visit.
import { canDraw } from "../hall/webgl";

const STORE = "pinakes.layout";
type Layout = "hall" | "list";

function saved(): Layout | null {
  try {
    const v = localStorage.getItem(STORE);
    return v === "hall" || v === "list" ? v : null;
  } catch {
    return null;
  }
}

class LayoutState {
  chosen = $state<Layout>(saved() ?? (window.innerWidth >= 760 ? "hall" : "list"));
  failed = $state(false);

  get hall(): boolean {
    return this.chosen === "hall" && !this.failed && canDraw();
  }

  set(l: Layout): void {
    this.chosen = l;
    if (l === "hall") this.failed = false;
    try {
      localStorage.setItem(STORE, l);
    } catch {
      // for this visit only
    }
  }

  /** The hall couldn't be drawn: the list, for this visit. */
  fail(): void {
    this.failed = true;
  }
}

export const layout = new LayoutState();
