<!-- Line icons drawn for the console (no emoji, no icon font). -->
<script lang="ts">
  type Name =
    | "overview" | "search" | "wiki" | "knowledge" | "graph" | "review" | "lint" | "acquisition"
    | "admin" | "settings" | "sun" | "moon" | "out" | "close" | "upload"
    | "check" | "x" | "arrow" | "external" | "refresh" | "plus" | "trash" | "history" | "edit"
    | "columns" | "list";
  let { name, size = 18 }: { name: Name; size?: number } = $props();
  const paths: Record<Name, string> = {
    overview: "M4 5h7v6H4zM13 5h7v3h-7zM13 10h7v9h-7zM4 13h7v6H4z",
    search: "M10.5 4a6.5 6.5 0 1 1 0 13 6.5 6.5 0 0 1 0-13zM15.5 15.5 20 20",
    wiki: "M5 4h9l5 5v11H5zM14 4v5h5M8 13h8M8 16h6",
    knowledge: "M4 19V5h6a2 2 0 0 1 2 2v12a2 2 0 0 0-2-2H4zM20 19V5h-6a2 2 0 0 0-2 2",
    graph: "M6 6m-2 0a2 2 0 1 0 4 0a2 2 0 1 0-4 0M18 7m-2 0a2 2 0 1 0 4 0a2 2 0 1 0-4 0M12 18m-2 0a2 2 0 1 0 4 0a2 2 0 1 0-4 0M8 6.5l8 .5M7 8l4 8M17 9l-4 7",
    review: "M5 4h14v16l-7-4-7 4zM9 9l2 2 4-4",
    lint: "M12 3l8 4v5c0 5-3.5 8-8 9-4.5-1-8-4-8-9V7zM9 12l2 2 4-4",
    acquisition: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM3 12h18M12 3c2.5 2.7 3.5 5.7 3.5 9s-1 6.3-3.5 9c-2.5-2.7-3.5-5.7-3.5-9s1-6.3 3.5-9",
    admin: "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM4 20c1.5-3.5 4.5-5 8-5s6.5 1.5 8 5",
    settings: "M4 7h10M18 7h2M4 17h4M12 17h8M14 4v6M8 14v6",
    sun: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4",
    moon: "M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z",
    out: "M14 4h5v16h-5M10 8l-4 4 4 4M6 12h10",
    close: "M6 6l12 12M18 6 6 18",
    upload: "M12 16V4M7 9l5-5 5 5M5 20h14",
    check: "M5 12l5 5 9-10",
    x: "M6 6l12 12M18 6 6 18",
    arrow: "M5 12h14M13 6l6 6-6 6",
    external: "M14 4h6v6M20 4l-9 9M18 14v6H4V6h6",
    refresh: "M20 6v5h-5M4 18v-5h5M18.5 11A7 7 0 0 0 6 7.5L4 9.5M5.5 13A7 7 0 0 0 18 16.5l2-2",
    plus: "M12 5v14M5 12h14",
    trash: "M5 7h14M9 7V4h6v3M7 7l1 13h8l1-13",
    history: "M4 12a8 8 0 1 0 2.5-5.8L4 8.5M4 4v4.5h4.5M12 8v4l3 2",
    edit: "M4 20h4L19 9l-4-4L4 16zM13.5 6.5l4 4",
    columns: "M3 9l9-5 9 5zM5 9v9M9.5 9v9M14.5 9v9M19 9v9M3 20h18",
    list: "M8 6h12M8 12h12M8 18h12M4 6h.01M4 12h.01M4 18h.01",
  };
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.6"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"><path d={paths[name]} /></svg>
