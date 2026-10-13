// A graph draft as a picture: nodes put (added, or changed when already in the graph),
// removed nodes as ghosts, edges added and removed; ends an edge names but the draft doesn't
// list are drawn plainly so the edge has somewhere to go.
import type { GraphDraft } from "../api/types";
import type { GEdge, GNode } from "../components/GraphCanvas.svelte";

export function draftGraph(d: GraphDraft, existing: Set<string> = new Set()): { nodes: GNode[]; edges: GEdge[] } {
  const nodes = new Map<string, GNode>();
  const id = (space: string, type: string, key: string) => `${space}:${type}/${key}`;
  for (const n of d.put_nodes) {
    const i = id(n.space, n.type, n.key);
    nodes.set(i, { id: i, label: n.name || n.key, type: n.type, mark: existing.has(i) ? "changed" : "added" });
  }
  for (const n of d.remove_nodes) {
    const i = id(n.space, n.type, n.key);
    nodes.set(i, { id: i, label: n.key, type: n.type, mark: "removed" });
  }
  const edges: GEdge[] = [];
  const end = (space: string, type: string, key: string) => {
    const i = id(space, type, key);
    if (!nodes.has(i)) nodes.set(i, { id: i, label: key, type, mark: "normal" });
    return i;
  };
  for (const e of d.add_edges) {
    edges.push({ from: end(e.space, e.from_type, e.from_key), to: end(e.space, e.to_type, e.to_key), type: e.type, mark: "added" });
  }
  for (const e of d.remove_edges) {
    edges.push({ from: end(e.space, e.from_type, e.from_key), to: end(e.space, e.to_type, e.to_key), type: e.type, mark: "removed" });
  }
  return { nodes: [...nodes.values()], edges };
}

/** How many changes a draft holds (deleting the space counts as one). */
export function draftSize(d: GraphDraft): number {
  return (d.delete_space ? 1 : 0) + d.put_nodes.length + d.remove_nodes.length + d.add_edges.length + d.remove_edges.length;
}

/** Nodes (all types) and edges a listed space holds, for saying what deleting it removes. */
export function spaceCounts(s: { nodes: Record<string, number>; edges: number } | undefined): { nodes: number; edges: number } | null {
  if (!s) return null;
  return { nodes: Object.values(s.nodes).reduce((a, b) => a + b, 0), edges: s.edges };
}
