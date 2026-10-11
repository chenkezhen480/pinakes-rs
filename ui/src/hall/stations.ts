// The hall's stations: each of the console's sections stands somewhere in the Library, as the
// thing it is in Alexandria. Shared by the scene (which places and draws them) and the page
// around it (labels, the dock), so the list lives without three.js.

export const STATIONS = [
  // The inscribed stele by the entrance: what the library holds.
  "overview",
  // Callimachus' catalogue: the long table of pinakes.
  "search",
  // The armaria: shelves of scrolls along the left wall.
  "wiki",
  // The scriptorium desk, where new rolls are copied in.
  "knowledge",
  // The armillary sphere before the middle window: how things relate.
  "graph",
  // The lectern with the seal: what waits for approval.
  "review",
  // The correctors' table under the lamps.
  "lint",
  // The far window, looking out over the clouds, with a ship model by it (ships once brought
  // Alexandria its books).
  "acquisition",
  // The bust of the librarian: tenants and their people.
  "admin",
  // Ctesibius' water clock: how the library runs.
  "settings",
] as const;

export type Station = (typeof STATIONS)[number];

export function isStation(s: string): s is Station {
  return (STATIONS as readonly string[]).includes(s);
}
