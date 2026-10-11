// The sky outside the hall's windows, from the local clock: early morning light through the day
// (or a grey, rainy day, about three days in ten, picked by the date), a sunset in the evening,
// and stars and the moon at night. Pure: the time in, colours and amounts out, blended between key hours so
// the light moves as the day does.

export type Weather = "clear" | "rain";
export type Phase = "night" | "dawn" | "day" | "rain" | "dusk";

export interface Sky {
  phase: Phase;
  /** Colours as 0xRRGGBB. */
  zenith: number;
  horizon: number;
  /** The shade of the clouds below. */
  below: number;
  sun: number;
  /** The sun (or moon) light's strength, the hall's ambient light, their colours. */
  sunLight: number;
  ambient: number;
  ambientColor: number;
  /** Degrees above the horizon, and across the windows (-1 left, 1 right). */
  elevation: number;
  across: number;
  /** 0..1 amounts. */
  stars: number;
  clouds: number;
  cloudColor: number;
  /** The hall's oil lamps (lit as the light goes). */
  lamps: number;
  /** Shafts of light through the windows. */
  shafts: number;
  rain: number;
}

type Key = Omit<Sky, "phase" | "elevation" | "across">;

const NIGHT: Key = {
  zenith: 0x060b1d, horizon: 0x141d3a, below: 0x1a2340, sun: 0xa9bce8, sunLight: 0.35,
  ambient: 0.3, ambientColor: 0x3a4a7a, stars: 1, clouds: 0.15, cloudColor: 0x46557e,
  lamps: 1, shafts: 0, rain: 0,
};
const DAWN: Key = {
  zenith: 0x46609f, horizon: 0xeea083, below: 0x8a6a80, sun: 0xffc59a, sunLight: 1.3,
  ambient: 0.7, ambientColor: 0xd9c2b4, stars: 0.15, clouds: 0.35, cloudColor: 0xffd9c2,
  lamps: 0.45, shafts: 0.9, rain: 0,
};
const MORNING: Key = {
  zenith: 0x3a74c4, horizon: 0x9fbfe4, below: 0x8ea4c0, sun: 0xfff0d6, sunLight: 2.0,
  ambient: 1.0, ambientColor: 0xe9e2d4, stars: 0, clouds: 0.3, cloudColor: 0xffffff,
  lamps: 0, shafts: 0.65, rain: 0,
};
const RAIN: Key = {
  zenith: 0x56606e, horizon: 0x9aa3ad, below: 0x6b7480, sun: 0xd2d8de, sunLight: 0.7,
  ambient: 0.9, ambientColor: 0x9aa6b4, stars: 0, clouds: 1, cloudColor: 0x7d8692,
  lamps: 0.3, shafts: 0, rain: 1,
};
const SUNSET: Key = {
  zenith: 0x2e3a74, horizon: 0xff8a3d, below: 0x7a4458, sun: 0xff9a52, sunLight: 1.7,
  ambient: 0.65, ambientColor: 0xe0a07c, stars: 0.05, clouds: 0.45, cloudColor: 0xffb48c,
  lamps: 0.6, shafts: 1, rain: 0,
};
const GLOAMING: Key = {
  zenith: 0x141c3e, horizon: 0x6a4a6e, below: 0x302a4a, sun: 0xc28aa0, sunLight: 0.5,
  ambient: 0.45, ambientColor: 0x6e6488, stars: 0.6, clouds: 0.3, cloudColor: 0x8f6c8e,
  lamps: 0.9, shafts: 0.15, rain: 0,
};

/** Hours of the day and the sky they have; between two, a blend. */
function keys(weather: Weather): [number, Key][] {
  const day = weather === "rain" ? RAIN : MORNING;
  const dawn = weather === "rain" ? mix(DAWN, RAIN, 0.6) : DAWN;
  return [
    [0, NIGHT],
    [4.6, NIGHT],
    [6.0, dawn],
    [8.0, day],
    [16.0, day],
    [17.6, SUNSET],
    [18.8, GLOAMING],
    [20.0, NIGHT],
    [24, NIGHT],
  ];
}

function lerp(a: number, b: number, t: number): number {
  return a + (b - a) * t;
}

function lerpColor(a: number, b: number, t: number): number {
  const ch = (c: number, s: number) => (c >> s) & 0xff;
  const r = Math.round(lerp(ch(a, 16), ch(b, 16), t));
  const g = Math.round(lerp(ch(a, 8), ch(b, 8), t));
  const bl = Math.round(lerp(ch(a, 0), ch(b, 0), t));
  return (r << 16) | (g << 8) | bl;
}

const COLORS = new Set<keyof Key>(["zenith", "horizon", "below", "sun", "ambientColor", "cloudColor"]);

function mix(a: Key, b: Key, t: number): Key {
  const out = { ...a };
  for (const k of Object.keys(a) as (keyof Key)[]) {
    out[k] = COLORS.has(k) ? lerpColor(a[k], b[k], t) : lerp(a[k], b[k], t);
  }
  return out;
}

/** A day's weather, the same all day: about three days in ten rain. */
export function weatherOf(date: Date): Weather {
  const s = `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619) >>> 0;
  return h % 10 < 3 ? "rain" : "clear";
}

/** The sky at a time of day (hours, 0..24) in a weather. */
export function skyAtHour(hour: number, weather: Weather): Sky {
  const ks = keys(weather);
  let i = 0;
  while (i < ks.length - 2 && (ks[i + 1]?.[0] ?? 24) <= hour) i++;
  const [h0, k0] = ks[i] ?? [0, NIGHT];
  const [h1, k1] = ks[i + 1] ?? [24, NIGHT];
  const t = h1 === h0 ? 0 : Math.min(1, Math.max(0, (hour - h0) / (h1 - h0)));
  // Smooth the blend, so a key hour is a plateau rather than a corner.
  const k = mix(k0, k1, t * t * (3 - 2 * t));
  // The sun rises at the left window at six and sets at the right one at half past six; at
  // night the moon stands high on the left.
  const dayT = (hour - 6) / 12.5;
  const up = dayT >= 0 && dayT <= 1;
  const elevation = up ? Math.sin(Math.PI * dayT) * 55 : 30;
  const across = up ? lerp(-0.9, 0.9, dayT) : -0.45;
  const phase: Phase =
    k.stars > 0.5 ? "night" : hour >= 15.5 && hour < 20 ? "dusk" : hour < 7.5 ? "dawn" : weather === "rain" ? "rain" : "day";
  return { ...k, phase, elevation, across };
}

/** A sky named for a look at it (`?sky=` on the hall): dawn, day, rain, dusk or night. */
export function namedSky(name: string): Sky | null {
  switch (name) {
    case "dawn":
      return skyAtHour(6.35, "clear");
    case "day":
      return skyAtHour(10, "clear");
    case "rain":
      return skyAtHour(11, "rain");
    case "dusk":
      return skyAtHour(18.1, "clear");
    case "night":
      return skyAtHour(22, "clear");
    default:
      return null;
  }
}

/** The sky now. */
export function skyNow(now = new Date()): Sky {
  return skyAtHour(now.getHours() + now.getMinutes() / 60, weatherOf(now));
}
