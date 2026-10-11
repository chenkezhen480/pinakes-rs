// The hall of the Library, drawn with three.js: a colonnaded reading room of limestone and
// marble under a coffered ceiling of lapis and gold stars, scrolls in cedar armaria along the
// walls, and at the far end three tall arches open on a terrace, the sea and the sky, and nothing
// else (the sky is the picture). The console's sections stand in it as the things they are in Alexandria (`stations.ts`);
// the camera walks to one when it is chosen. Loaded on demand (its own chunk), only when the
// browser can draw WebGL.

import * as THREE from "three";
import { EffectComposer } from "three/addons/postprocessing/EffectComposer.js";
import { RenderPass } from "three/addons/postprocessing/RenderPass.js";
import { UnrealBloomPass } from "three/addons/postprocessing/UnrealBloomPass.js";
import { OutputPass } from "three/addons/postprocessing/OutputPass.js";
import { ShaderPass } from "three/addons/postprocessing/ShaderPass.js";
import type { Sky } from "./sky";
import { isStation, type Station } from "./stations";
import * as tex from "./textures";

export interface HallOptions {
  canvas: HTMLCanvasElement;
  /** Under reduced motion the camera cuts and nothing drifts. */
  motion: boolean;
  onPick: (station: Station) => void;
  onHover: (station: Station | null) => void;
  /** The WebGL context was lost: the hall can't draw any more. */
  onLost: () => void;
}

export interface Anchor {
  station: Station;
  x: number;
  y: number;
  /** In front of the camera and inside the view. */
  visible: boolean;
}

export interface Hall {
  setSky(sky: Sky): void;
  /** Walks to a station (or back to the entrance with `null`); `panel` is how many pixels a
   *  sheet covers on the right, so the station is framed in what is left. */
  focus(station: Station | null, panel: number): void;
  /** Where each station's label goes, in CSS pixels; called every frame. */
  onFrame(cb: (anchors: Anchor[]) => void): void;
  highlight(station: Station | null): void;
  /** Hidden behind a page that covers the window: nothing is drawn until it shows again. */
  cover(covered: boolean): void;
  dispose(): void;
}

// The hall's plan, in metres: x across (-13..13), y up (0..13), z along (16 at the entrance,
// -22 at the window wall, whose three tall arches frame the sky).
const HALL = { x: 13, near: 16, far: -22, height: 13 };
const COLUMN_Z = [10, 4, -2, -8, -14];
const COLUMN_X = 7.5;
const ARCHES = [-8.6, 0, 8.6];
const ARCH = { half: 3.3, bottom: 0.9, spring: 8.3 };
const KINDS = [0xb5612d, 0x7a4e9c, 0x2f6f8a, 0x3f7d4e, 0xa3476b];

interface Pose {
  position: THREE.Vector3;
  target: THREE.Vector3;
}

const v = (x: number, y: number, z: number) => new THREE.Vector3(x, y, z);

const HOME: Pose = { position: v(0, 5.2, 15.2), target: v(0, 4.2, -22) };

/** Where each station stands, where its label floats, and where the camera looks at it from. */
const PLACES: Record<Station, { at: THREE.Vector3; label: THREE.Vector3; pose: Pose }> = {
  overview: { at: v(-4.4, 0, 6), label: v(-4.4, 4.4, 6), pose: { position: v(-0.4, 3.3, 9.2), target: v(-4.4, 2.3, 6) } },
  admin: { at: v(4.4, 0, 6), label: v(4.4, 3.4, 6), pose: { position: v(0.6, 3.1, 9.2), target: v(4.4, 2.2, 6) } },
  search: { at: v(0, 0, -1), label: v(0, 2.6, -1), pose: { position: v(3.4, 3.7, 5.2), target: v(0, 1.0, -1.0) } },
  wiki: { at: v(-12.4, 0, -5), label: v(-11.6, 6.4, -5), pose: { position: v(-2.8, 4.0, -4.6), target: v(-12.4, 3.3, -5.2) } },
  review: { at: v(-9.6, 0, 4.5), label: v(-9.6, 2.9, 4.5), pose: { position: v(-5.6, 3.4, 7.4), target: v(-9.6, 1.5, 4.4) } },
  knowledge: { at: v(9.6, 0, 4.5), label: v(9.6, 2.9, 4.5), pose: { position: v(5.6, 3.4, 7.4), target: v(9.6, 1.4, 4.4) } },
  lint: { at: v(9.6, 0, -11), label: v(9.6, 2.9, -11), pose: { position: v(5.6, 3.3, -7.6), target: v(9.6, 1.3, -11) } },
  graph: { at: v(0, 0, -14.5), label: v(0, 4.6, -14.5), pose: { position: v(3.0, 3.7, -7.0), target: v(0, 2.6, -14.5) } },
  acquisition: { at: v(-8.4, 0, -19.4), label: v(-8.4, 3.0, -19.4), pose: { position: v(-4.6, 3.4, -13.6), target: v(-9.4, 2.4, -23) } },
  settings: { at: v(8.4, 0, -19.4), label: v(8.4, 3.9, -19.4), pose: { position: v(4.6, 3.4, -13.8), target: v(8.4, 2.0, -19.4) } },
};

function canvasTexture(c: HTMLCanvasElement, repeat: [number, number] = [1, 1], srgb = true): THREE.CanvasTexture {
  const t = new THREE.CanvasTexture(c);
  if (srgb) t.colorSpace = THREE.SRGBColorSpace;
  t.wrapS = THREE.RepeatWrapping;
  t.wrapT = THREE.RepeatWrapping;
  t.repeat.set(repeat[0], repeat[1]);
  return t;
}

/** A unit box placed and sized, for building furniture from boards. */
function board(mat: THREE.Material, size: [number, number, number], at: [number, number, number], parent: THREE.Object3D, shadows = true): THREE.Mesh {
  const m = new THREE.Mesh(new THREE.BoxGeometry(...size), mat);
  m.position.set(...at);
  m.castShadow = shadows;
  m.receiveShadow = true;
  parent.add(m);
  return m;
}

function lathe(profile: [number, number][], mat: THREE.Material, segments = 24): THREE.Mesh {
  const g = new THREE.LatheGeometry(profile.map(([r, y]) => new THREE.Vector2(r, y)), segments);
  const m = new THREE.Mesh(g, mat);
  m.castShadow = true;
  m.receiveShadow = true;
  return m;
}

const SKY_VERTEX = /* glsl */ `
varying vec3 vDir;
void main() {
  vec4 wp = modelMatrix * vec4(position, 1.0);
  vDir = wp.xyz - cameraPosition;
  gl_Position = projectionMatrix * viewMatrix * wp;
  gl_Position.z = gl_Position.w;
}`;

const SKY_FRAGMENT = /* glsl */ `
uniform vec3 zenith;
uniform vec3 horizon;
uniform vec3 sunColor;
uniform vec3 sunDir;
uniform float sunDisk;
uniform float sunGlow;
varying vec3 vDir;
void main() {
  vec3 d = normalize(vDir);
  float h = clamp(d.y, 0.0, 1.0);
  vec3 col = mix(horizon, zenith, pow(h, 0.5));
  if (d.y < 0.0) col = horizon * (0.92 + 0.08 * (1.0 + d.y));
  float s = max(dot(d, sunDir), 0.0);
  col += sunColor * (pow(s, 6.0) * 0.35 * sunGlow + pow(s, 30.0) * 0.4 * sunGlow);
  col += sunColor * smoothstep(0.9993, 0.9997, s) * 3.0 * sunDisk;
  gl_FragColor = vec4(col, 1.0);
  #include <tonemapping_fragment>
  #include <colorspace_fragment>
}`;

// The sea of clouds under the terrace: two layers of value noise drifting at different speeds,
// lit on the side facing the sun and shaded in the hollows, fading into the horizon's haze.
const CLOUDS_VERTEX = /* glsl */ `
varying vec3 vWorld;
void main() {
  vec4 wp = modelMatrix * vec4(position, 1.0);
  vWorld = wp.xyz;
  gl_Position = projectionMatrix * viewMatrix * wp;
}`;

const CLOUDS_FRAGMENT = /* glsl */ `
uniform vec3 top;
uniform vec3 shade;
uniform vec3 horizon;
uniform vec3 sunColor;
uniform vec3 sunDir;
uniform float sunLight;
uniform float time;
varying vec3 vWorld;
float hash(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}
float noise(vec2 p) {
  vec2 i = floor(p);
  vec2 f = fract(p);
  vec2 u = f * f * (3.0 - 2.0 * f);
  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x), mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}
float fbm(vec2 p) {
  float v = 0.0;
  float a = 0.5;
  for (int i = 0; i < 6; i++) {
    v += a * noise(p);
    p = p * 2.03 + vec2(1.7, 9.2);
    a *= 0.5;
  }
  return v;
}
void main() {
  vec2 p = vWorld.xz * 0.011;
  vec2 drift = vec2(time * 0.006, time * 0.002);
  float d = fbm(p + drift) * 0.65 + fbm(p * 2.1 - drift * 1.7 + 4.0) * 0.35;
  // Light from the sun's side: the noise a step toward the sun, compared with here.
  vec2 toward = normalize(sunDir.xz + vec2(0.0001)) * 0.06;
  float ahead = fbm(p + drift + toward) * 0.65 + fbm((p + toward) * 2.1 - drift * 1.7 + 4.0) * 0.35;
  float lit = clamp(0.55 + (d - ahead) * 5.0, 0.0, 1.0);
  float billow = smoothstep(0.32, 0.72, d);
  vec3 col = mix(shade, top, pow(billow * 0.6 + lit * 0.4, 1.4));
  // The sun's glow over the clouds toward it.
  vec3 v = normalize(vWorld - cameraPosition);
  float s = max(dot(normalize(vec3(v.x, 0.0, v.z)), normalize(vec3(sunDir.x, 0.0, sunDir.z))), 0.0);
  col += sunColor * pow(s, 10.0) * 0.35 * sunLight * (0.4 + billow);
  float dist = length(vWorld.xz - cameraPosition.xz);
  col = mix(col, horizon, smoothstep(250.0, 2200.0, dist) * 0.75);
  gl_FragColor = vec4(col, 1.0);
  #include <tonemapping_fragment>
  #include <colorspace_fragment>
}`;

const FINISH = {
  uniforms: {
    tDiffuse: { value: null },
    time: { value: 0 },
    center: { value: 0.5 },
    vignette: { value: 0.34 },
    grain: { value: 0.03 },
  },
  vertexShader: /* glsl */ `
varying vec2 vUv;
void main() {
  vUv = uv;
  gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
}`,
  fragmentShader: /* glsl */ `
uniform sampler2D tDiffuse;
uniform float time;
uniform float center;
uniform float vignette;
uniform float grain;
varying vec2 vUv;
float rand(vec2 co) {
  return fract(sin(dot(co, vec2(12.9898, 78.233))) * 43758.5453);
}
void main() {
  vec4 c = texture2D(tDiffuse, vUv);
  vec2 d = (vUv - vec2(center, 0.5)) * vec2(1.0, 0.8);
  float v = smoothstep(0.95, 0.2, length(d));
  c.rgb *= mix(1.0 - vignette, 1.0, v);
  c.rgb += (rand(vUv * 731.0 + fract(time * 7.13)) - 0.5) * grain;
  gl_FragColor = c;
}`,
};

export function createHall(opts: HallOptions): Hall {
  const { canvas } = opts;
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: true, powerPreference: "high-performance" });
  const info = renderer.getContext();
  const debug = info.getExtension("WEBGL_debug_renderer_info");
  const gpu = debug ? String(info.getParameter(debug.UNMASKED_RENDERER_WEBGL)) : "";
  // A software renderer (a virtual machine, a test browser) gets no shadows and fewer pixels.
  const soft = /swiftshader|llvmpipe|software/i.test(gpu);
  // A software renderer draws only when something changes: the camera cuts, nothing drifts.
  const motion = opts.motion && !soft;
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, soft ? 1 : 1.75));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.05;
  renderer.shadowMap.enabled = !soft;
  // Shader logs only while developing (a driver's harmless notes would read as warnings).
  renderer.debug.checkShaderErrors = import.meta.env.DEV;
  renderer.shadowMap.type = THREE.PCFShadowMap;
  const aniso = Math.min(8, renderer.capabilities.getMaxAnisotropy());

  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(52, 1, 0.1, 3000);
  camera.position.copy(HOME.position);
  const lookAt = HOME.target.clone();
  camera.lookAt(lookAt);

  const disposables: { dispose(): void }[] = [];
  const own = <T extends { dispose(): void }>(x: T): T => {
    disposables.push(x);
    return x;
  };
  const texture = (c: HTMLCanvasElement, repeat: [number, number] = [1, 1]) => {
    const t = own(canvasTexture(c, repeat));
    t.anisotropy = aniso;
    return t;
  };

  // ---- Materials ----------------------------------------------------------------------------
  const limestoneCanvas = tex.limestone();
  const marbleCanvas = tex.marble("#ece3d1", "#9d8a6e", 5);
  const woodCanvas = tex.wood();
  const papyrusCanvas = tex.papyrus();
  const walls = texture(limestoneCanvas, [7, 2.2]);
  const windowStone = texture(limestoneCanvas, [1 / 6, 1 / 3.2]);
  const marbleTex = texture(marbleCanvas);
  const woodTex = texture(woodCanvas);
  const darkWoodTex = texture(tex.wood(9, 512, "#3e2414"));
  const mat = {
    wall: own(new THREE.MeshStandardMaterial({ map: walls, bumpMap: walls, bumpScale: 2.2, roughness: 0.92 })),
    windowWall: own(new THREE.MeshStandardMaterial({ map: windowStone, bumpMap: windowStone, bumpScale: 2.2, roughness: 0.92 })),
    marble: own(new THREE.MeshStandardMaterial({ map: marbleTex, bumpMap: marbleTex, bumpScale: 0.3, roughness: 0.32 })),
    column: own(new THREE.MeshStandardMaterial({ map: texture(tex.fluted(), [1, 1.6]), bumpMap: texture(tex.flutesBump(), [1, 1]), bumpScale: 6, roughness: 0.36 })),
    wood: own(new THREE.MeshStandardMaterial({ map: woodTex, bumpMap: woodTex, bumpScale: 1.2, roughness: 0.62 })),
    darkWood: own(new THREE.MeshStandardMaterial({ map: darkWoodTex, bumpMap: darkWoodTex, bumpScale: 1.2, roughness: 0.68 })),
    papyrus: own(new THREE.MeshStandardMaterial({ map: texture(papyrusCanvas), roughness: 0.95 })),
    bronze: own(new THREE.MeshStandardMaterial({ color: 0xc08a45, metalness: 0.9, roughness: 0.3 })),
    darkBronze: own(new THREE.MeshStandardMaterial({ color: 0x7a5530, metalness: 0.85, roughness: 0.4 })),
    lapis: own(new THREE.MeshStandardMaterial({ color: 0x24457e, roughness: 0.35 })),
    seal: own(new THREE.MeshStandardMaterial({ color: 0xa8321f, roughness: 0.5 })),
    clay: own(new THREE.MeshStandardMaterial({ map: texture(tex.tablet()), roughness: 0.85 })),
    water: own(new THREE.MeshStandardMaterial({ color: 0x6fa7c9, roughness: 0.1, metalness: 0.1, transparent: true, opacity: 0.55 })),
    stoneOut: own(new THREE.MeshStandardMaterial({ color: 0xd9ccb0, roughness: 0.9 })),
    sail: own(new THREE.MeshStandardMaterial({ color: 0xeee2c4, roughness: 0.9, side: THREE.DoubleSide })),
    terracotta: own(new THREE.MeshStandardMaterial({ map: texture(tex.amphora()), roughness: 0.55 })),
    velvet: own(new THREE.MeshPhysicalMaterial({ map: texture(tex.velvet("#5e1419", "#c99a3c")), roughness: 0.85, sheen: 1, sheenColor: new THREE.Color(0xd4766a), sheenRoughness: 0.45, side: THREE.DoubleSide })),
    pick: own(new THREE.MeshBasicMaterial({ visible: false })),
  };

  // ---- The room -----------------------------------------------------------------------------
  const room = new THREE.Group();
  scene.add(room);
  const width = HALL.x * 2;
  const length = HALL.near - HALL.far;
  const midZ = (HALL.near + HALL.far) / 2;

  const floorTex = texture(tex.floor(width, HALL.near, HALL.far, [PLACES.graph.at.z, PLACES.search.at.z]));
  floorTex.flipY = false;
  floorTex.wrapS = THREE.ClampToEdgeWrapping;
  floorTex.wrapT = THREE.ClampToEdgeWrapping;
  const floorMat = own(new THREE.MeshStandardMaterial({ map: floorTex, bumpMap: floorTex, bumpScale: 0.8, roughness: 0.42, metalness: 0.02 }));
  const floor = new THREE.Mesh(own(new THREE.PlaneGeometry(width, length)), floorMat);
  floor.rotation.x = -Math.PI / 2;
  floor.position.set(0, 0, midZ);
  floor.receiveShadow = true;
  room.add(floor);

  const coffersTex = texture(tex.coffers(), [width / 3, length / 3]);
  const ceilingMat = own(new THREE.MeshStandardMaterial({ map: coffersTex, bumpMap: coffersTex, bumpScale: 4, roughness: 0.7 }));
  const ceiling = new THREE.Mesh(own(new THREE.PlaneGeometry(width, length)), ceilingMat);
  ceiling.rotation.x = Math.PI / 2;
  ceiling.position.set(0, HALL.height, midZ);
  ceiling.castShadow = true;
  room.add(ceiling);

  for (const side of [-1, 1]) {
    const wall = new THREE.Mesh(own(new THREE.PlaneGeometry(length, HALL.height)), mat.wall);
    wall.rotation.y = side * -Math.PI / 2;
    wall.position.set(side * HALL.x, HALL.height / 2, midZ);
    wall.receiveShadow = true;
    wall.castShadow = true;
    room.add(wall);
    // The frieze under the ceiling.
    const frieze = new THREE.Mesh(
      own(new THREE.PlaneGeometry(length, 0.9)),
      own(new THREE.MeshStandardMaterial({ map: texture(tex.meander("#c99a3c", "#1e3d6b"), [length / 7, 1]), roughness: 0.6 })),
    );
    frieze.rotation.y = side * -Math.PI / 2;
    frieze.position.set(side * (HALL.x - 0.02), HALL.height - 0.75, midZ);
    room.add(frieze);
  }
  const entrance = new THREE.Mesh(own(new THREE.PlaneGeometry(width, HALL.height)), mat.wall);
  entrance.rotation.y = Math.PI;
  entrance.position.set(0, HALL.height / 2, HALL.near);
  room.add(entrance);

  // The window wall: three arches cut through it, each with an archivolt and a sill.
  const outline = new THREE.Shape();
  outline.moveTo(-HALL.x, 0);
  outline.lineTo(HALL.x, 0);
  outline.lineTo(HALL.x, HALL.height);
  outline.lineTo(-HALL.x, HALL.height);
  outline.lineTo(-HALL.x, 0);
  for (const cx of ARCHES) {
    const hole = new THREE.Path();
    hole.moveTo(cx - ARCH.half, ARCH.bottom);
    hole.lineTo(cx - ARCH.half, ARCH.spring);
    hole.absarc(cx, ARCH.spring, ARCH.half, Math.PI, 0, true);
    hole.lineTo(cx + ARCH.half, ARCH.bottom);
    hole.lineTo(cx - ARCH.half, ARCH.bottom);
    outline.holes.push(hole);
  }
  const windowWall = new THREE.Mesh(own(new THREE.ExtrudeGeometry(outline, { depth: 1.2, bevelEnabled: false, curveSegments: 24 })), mat.windowWall);
  windowWall.position.set(0, 0, HALL.far - 1.2);
  windowWall.castShadow = true;
  windowWall.receiveShadow = true;
  room.add(windowWall);
  for (const cx of ARCHES) {
    const arc = new THREE.Mesh(own(new THREE.TorusGeometry(ARCH.half + 0.18, 0.16, 8, 32, Math.PI)), mat.marble);
    arc.position.set(cx, ARCH.spring, HALL.far + 0.05);
    arc.castShadow = true;
    room.add(arc);
    for (const s of [-1, 1]) board(mat.marble, [0.34, ARCH.spring - ARCH.bottom, 0.3], [cx + s * (ARCH.half + 0.18), (ARCH.spring + ARCH.bottom) / 2, HALL.far + 0.05], room);
    board(mat.marble, [ARCH.half * 2 + 0.8, 0.22, 1.5], [cx, ARCH.bottom - 0.11, HALL.far - 0.6], room);
  }

  // Columns: base, fluted shaft, capital, and the beam they carry.
  const columnGeo = own(new THREE.CylinderGeometry(0.46, 0.54, 10.2, 24, 1, true));
  const baseGeo = own(new THREE.BoxGeometry(1.4, 0.45, 1.4));
  const capGeo = own(new THREE.CylinderGeometry(0.72, 0.48, 0.5, 24));
  const abacusGeo = own(new THREE.BoxGeometry(1.5, 0.28, 1.5));
  for (const sx of [-1, 1]) {
    for (const z of COLUMN_Z) {
      const x = sx * COLUMN_X;
      const base = new THREE.Mesh(baseGeo, mat.marble);
      base.position.set(x, 0.225, z);
      const shaft = new THREE.Mesh(columnGeo, mat.column);
      shaft.position.set(x, 0.45 + 5.1, z);
      const cap = new THREE.Mesh(capGeo, mat.marble);
      cap.position.set(x, 10.9, z);
      const abacus = new THREE.Mesh(abacusGeo, mat.marble);
      abacus.position.set(x, 11.29, z);
      for (const m of [base, shaft, cap, abacus]) {
        m.castShadow = true;
        m.receiveShadow = true;
        room.add(m);
      }
    }
    board(mat.marble, [1.1, 1.2, length], [sx * COLUMN_X, 12.0, midZ], room);
  }

  // Armaria along both walls, between the columns: cedar pigeonholes full of scrolls with
  // coloured tags (the five kinds).
  const cabinetZ = [7, 1, -5, -11, -17];
  // (The last pair stands between the last columns and the window wall.)
  const boards: THREE.Matrix4[] = [];
  const scrolls: THREE.Matrix4[] = [];
  const scrollTints: THREE.Color[] = [];
  const tags: THREE.Matrix4[] = [];
  const tagColors: THREE.Color[] = [];
  const rnd = tex.seeded(77);
  const place = (list: THREE.Matrix4[], pos: THREE.Vector3, scale: THREE.Vector3, rot = new THREE.Quaternion()) =>
    list.push(new THREE.Matrix4().compose(pos, rot, scale));
  const along = new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(0, 0, 1), Math.PI / 2);
  const cols = 4;
  const rows = 6;
  const cw = 4.4;
  const cellW = cw / cols;
  const cellH = 1.08;
  const depth = 1.1;
  for (const side of [-1, 1]) {
    const x0 = side * (HALL.x - depth / 2);
    for (const z of cabinetZ) {
      const top = 0.5 + rows * cellH;
      place(boards, v(x0, top / 2, z - cw / 2 - 0.05), v(depth, top + 0.1, 0.1));
      place(boards, v(x0, top / 2, z + cw / 2 + 0.05), v(depth, top + 0.1, 0.1));
      place(boards, v(x0, top + 0.12, z), v(depth + 0.25, 0.24, cw + 0.5));
      place(boards, v(x0, 0.25, z), v(depth + 0.1, 0.5, cw + 0.2));
      for (let r = 1; r < rows; r++) place(boards, v(x0, 0.5 + r * cellH, z), v(depth, 0.06, cw));
      for (let c = 1; c < cols; c++) place(boards, v(x0, top / 2 + 0.25, z - cw / 2 + c * cellW), v(depth, top - 0.5, 0.06));
      place(boards, v(side * (HALL.x - 0.03), top / 2, z), v(0.04, top, cw));
      for (let r = 0; r < rows; r++) {
        for (let c = 0; c < cols; c++) {
          const cz = z - cw / 2 + (c + 0.5) * cellW;
          const cy = 0.5 + r * cellH + 0.04;
          // Scrolls stacked three, two, one, now and then a gap.
          const stack: [number, number][] = [[-0.26, 0.13], [0, 0.13], [0.26, 0.13], [-0.13, 0.355], [0.13, 0.355], [0, 0.58]];
          for (const [dz, dy] of stack) {
            if (rnd() < 0.12) continue;
            const len = 0.85 + rnd() * 0.15;
            place(scrolls, v(x0 + side * (0.02 + rnd() * 0.06), cy + dy, cz + dz), v(1, len, 1), along);
            const t = 0.82 + rnd() * 0.22;
            scrollTints.push(new THREE.Color(t, t * (0.95 + rnd() * 0.05), t * (0.82 + rnd() * 0.1)));
            if (rnd() < 0.45) {
              place(tags, v(x0 - side * (len / 2 + 0.005), cy + dy - 0.1, cz + dz), v(0.01, 0.16, 0.1));
              tagColors.push(new THREE.Color(KINDS[Math.floor(rnd() * KINDS.length)] ?? 0xb5612d));
            }
          }
        }
      }
    }
  }
  const instanced = (geo: THREE.BufferGeometry, material: THREE.Material, matrices: THREE.Matrix4[], colors?: THREE.Color[]) => {
    const m = new THREE.InstancedMesh(own(geo), material, matrices.length);
    matrices.forEach((mx, i) => m.setMatrixAt(i, mx));
    colors?.forEach((c, i) => m.setColorAt(i, c));
    m.castShadow = true;
    m.receiveShadow = true;
    room.add(m);
    return m;
  };
  instanced(new THREE.BoxGeometry(1, 1, 1), mat.darkWood, boards);
  instanced(new THREE.CylinderGeometry(0.11, 0.11, 1, 10), mat.papyrus, scrolls, scrollTints);
  instanced(new THREE.BoxGeometry(1, 1, 1), own(new THREE.MeshStandardMaterial({ roughness: 0.8 })), tags, tagColors);

  // ---- Stations -----------------------------------------------------------------------------
  const stationGroups = new Map<Station, THREE.Group>();
  const picks: THREE.Mesh[] = [];
  const rings = new Map<Station, THREE.Mesh>();
  const ringMat = own(new THREE.MeshBasicMaterial({ color: 0xffd9a0, transparent: true, opacity: 0, blending: THREE.AdditiveBlending, depthWrite: false }));
  const station = (id: Station, pick: [number, number, number], pickAt: [number, number, number] = [0, pick[1] / 2, 0]) => {
    const g = new THREE.Group();
    g.position.copy(PLACES[id].at);
    scene.add(g);
    const hit = new THREE.Mesh(own(new THREE.BoxGeometry(...pick)), mat.pick);
    hit.position.set(...pickAt);
    hit.userData.station = id;
    g.add(hit);
    picks.push(hit);
    const ring = new THREE.Mesh(own(new THREE.RingGeometry(1.1, 1.35, 48)), ringMat.clone());
    own(ring.material as THREE.Material);
    ring.rotation.x = -Math.PI / 2;
    ring.position.set(pickAt[0], 0.02, pickAt[2]);
    g.add(ring);
    rings.set(id, ring);
    stationGroups.set(id, g);
    return g;
  };
  const flames: { sprite: THREE.Sprite; base: number; phase: number }[] = [];
  const glowTex = texture(tex.glow(128, "rgba(255,240,200,1)", "rgba(255,170,80,0.45)"));
  const flame = (parent: THREE.Object3D, at: [number, number, number], size = 0.5) => {
    const s = new THREE.Sprite(own(new THREE.SpriteMaterial({ map: glowTex, color: new THREE.Color(2.4, 1.45, 0.62), blending: THREE.AdditiveBlending, depthWrite: false, transparent: true })));
    s.position.set(...at);
    s.scale.setScalar(size);
    parent.add(s);
    flames.push({ sprite: s, base: size, phase: Math.random() * 10 });
    return s;
  };
  const oilLamp = (parent: THREE.Object3D, at: [number, number, number], flameOn = true) => {
    const lamp = lathe([[0, 0], [0.12, 0.01], [0.16, 0.05], [0.12, 0.09], [0.04, 0.1], [0, 0.1]], mat.bronze, 16);
    lamp.scale.set(1, 1, 1.5);
    lamp.position.set(...at);
    parent.add(lamp);
    if (flameOn) flame(parent, [at[0], at[1] + 0.2, at[2] + 0.2], 0.35);
  };

  // Overview: the stele, inscribed with the catalogue's full title.
  {
    const g = station("overview", [1.8, 4, 1.4]);
    board(mat.marble, [1.9, 0.5, 1.2], [0, 0.25, 0], g);
    const face = own(new THREE.MeshStandardMaterial({ map: texture(tex.inscription(["ΠΙΝΑΚΕΣ", "ΤΩΝ ΕΝ ΠΑΣΗΙ", "ΠΑΙΔΕΙΑΙ", "ΔΙΑΛΑΜΨΑΝΤΩΝ", "ΚΑΛΛΙΜΑΧΟΣ"])), roughness: 0.45 }));
    const slab = new THREE.Mesh(own(new THREE.BoxGeometry(1.4, 3.1, 0.36)), [mat.marble, mat.marble, mat.marble, mat.marble, face, mat.marble]);
    slab.position.set(0, 2.05, 0);
    slab.castShadow = true;
    slab.receiveShadow = true;
    g.add(slab);
    const pediment = new THREE.Mesh(own(new THREE.CylinderGeometry(0.01, 0.95, 0.5, 3, 1)), mat.marble);
    pediment.rotation.set(Math.PI / 2, 0, 0);
    pediment.scale.set(1, 0.4, 1);
    pediment.position.set(0, 3.75, 0);
    g.add(pediment);
    g.rotation.y = Math.PI / 2;
  }

  // Admin: the bust of the librarian (Demetrius of Phalerum, who founded the Library).
  {
    const g = station("admin", [1.4, 3.4, 1.4]);
    board(mat.marble, [0.95, 1.65, 0.95], [0, 0.82, 0], g);
    board(mat.marble, [1.15, 0.12, 1.15], [0, 1.7, 0], g);
    const chest = new THREE.Mesh(own(new THREE.SphereGeometry(0.42, 24, 16, 0, Math.PI * 2, 0, Math.PI / 2)), mat.marble);
    chest.scale.set(1.25, 0.9, 0.7);
    chest.position.set(0, 1.76, 0);
    const neck = new THREE.Mesh(own(new THREE.CylinderGeometry(0.11, 0.13, 0.28, 16)), mat.marble);
    neck.position.set(0, 2.25, 0);
    const head = new THREE.Mesh(own(new THREE.SphereGeometry(0.22, 24, 18)), mat.marble);
    head.scale.set(0.9, 1.15, 1);
    head.position.set(0, 2.55, 0.02);
    const beard = new THREE.Mesh(own(new THREE.SphereGeometry(0.15, 16, 12)), mat.marble);
    beard.scale.set(1, 1.1, 0.8);
    beard.position.set(0, 2.38, 0.1);
    const hair = new THREE.Mesh(own(new THREE.SphereGeometry(0.235, 24, 14, 0, Math.PI * 2, 0, Math.PI * 0.55)), mat.marble);
    hair.scale.set(0.95, 1.1, 1.05);
    hair.position.set(0, 2.58, -0.01);
    const nose = new THREE.Mesh(own(new THREE.ConeGeometry(0.035, 0.1, 6)), mat.marble);
    nose.rotation.x = Math.PI / 2 + 0.3;
    nose.position.set(0, 2.53, 0.22);
    const brow = new THREE.Mesh(own(new THREE.BoxGeometry(0.2, 0.03, 0.05)), mat.marble);
    brow.position.set(0, 2.6, 0.19);
    const drape = new THREE.Mesh(own(new THREE.TorusGeometry(0.33, 0.06, 8, 24, Math.PI)), mat.marble);
    drape.rotation.set(-0.9, 0, 0.5);
    drape.position.set(0.05, 1.95, 0.12);
    for (const m of [chest, neck, head, beard, hair, nose, brow, drape]) {
      m.castShadow = true;
      g.add(m);
    }
    g.rotation.y = -Math.PI / 2;
  }

  // Search: Callimachus' long catalogue table, piles of tablets, a chest of drawers, lamps.
  {
    const g = station("search", [7.8, 2.2, 2.8]);
    board(mat.wood, [7.6, 0.16, 2.4], [0, 1.02, 0], g);
    for (const sx of [-3.4, 3.4]) for (const sz of [-0.95, 0.95]) board(mat.darkWood, [0.16, 0.95, 0.16], [sx, 0.47, sz], g);
    board(mat.darkWood, [7, 0.1, 0.12], [0, 0.3, 0], g);
    const tabletGeo = own(new THREE.BoxGeometry(0.5, 0.05, 0.36));
    const r2 = tex.seeded(9);
    for (let p = 0; p < 7; p++) {
      const px = -3 + p * 0.95 + (r2() - 0.5) * 0.2;
      const pz = (r2() - 0.5) * 1.2;
      const n = 3 + Math.floor(r2() * 7);
      for (let i = 0; i < n; i++) {
        const t = new THREE.Mesh(tabletGeo, mat.clay);
        t.position.set(px + (r2() - 0.5) * 0.04, 1.125 + i * 0.052, pz + (r2() - 0.5) * 0.04);
        t.rotation.y = (r2() - 0.5) * 0.25;
        t.castShadow = true;
        g.add(t);
      }
    }
    // An open tablet propped up, the catalogue's page.
    const pinax = new THREE.Mesh(own(new THREE.BoxGeometry(0.9, 0.65, 0.04)), mat.clay);
    pinax.position.set(0.4, 1.45, -0.7);
    pinax.rotation.x = -0.35;
    pinax.castShadow = true;
    g.add(pinax);
    // The chest of drawers at the table's end.
    const chest = board(mat.wood, [1.0, 1.0, 1.6], [4.6, 0.5, 0], g);
    chest.rotation.y = 0;
    for (let i = 0; i < 3; i++) for (const s of [-0.38, 0.38]) board(mat.bronze, [0.04, 0.05, 0.14], [4.08, 0.25 + i * 0.3, s], g, false);
    oilLamp(g, [-1.6, 1.11, 0.5]);
    oilLamp(g, [2.2, 1.11, 0.45]);
  }

  // Review: the lectern with a roll awaiting the seal, the red wax and the stamp.
  {
    const g = station("review", [2, 2.4, 2]);
    board(mat.darkWood, [0.3, 1.2, 0.3], [0, 0.6, 0], g);
    board(mat.darkWood, [0.9, 0.1, 0.9], [0, 0.05, 0], g);
    const desk = board(mat.wood, [1.3, 0.08, 0.95], [0, 1.32, 0], g);
    desk.rotation.z = 0.35;
    const sheet = new THREE.Mesh(own(new THREE.PlaneGeometry(1.0, 0.7)), own(new THREE.MeshStandardMaterial({ map: texture(tex.writtenPapyrus(8)), roughness: 0.9 })));
    sheet.rotation.set(-Math.PI / 2, 0, 0);
    sheet.rotation.order = "ZXY";
    sheet.rotation.z = 0.35;
    sheet.position.set(0, 1.375, 0);
    g.add(sheet);
    const wax = lathe([[0, 0], [0.11, 0], [0.12, 0.02], [0.1, 0.05], [0, 0.06]], mat.seal);
    wax.position.set(0.15, 1.42, 0.25);
    wax.rotation.z = 0.35;
    g.add(wax);
    const stamp = lathe([[0, 0], [0.08, 0], [0.08, 0.03], [0.03, 0.05], [0.025, 0.22], [0.05, 0.27], [0, 0.29]], mat.bronze);
    stamp.position.set(-0.55, 0.02, 0.6);
    g.add(stamp);
    board(mat.darkWood, [0.5, 0.3, 0.45], [0.55, 0.15, 0.55], g);
    const sealed = new THREE.Mesh(own(new THREE.CylinderGeometry(0.06, 0.06, 0.45, 10)), mat.papyrus);
    sealed.rotation.z = Math.PI / 2;
    sealed.position.set(0.55, 0.36, 0.55);
    g.add(sealed);
    board(mat.seal, [0.03, 0.08, 0.08], [0.55, 0.36, 0.62], g, false);
    g.rotation.y = Math.PI / 2;
  }

  // Knowledge: the scriptorium desk, a roll being copied, ink and reed pen, a stool.
  {
    const g = station("knowledge", [2.4, 2.4, 2.2]);
    for (const sx of [-0.7, 0.7]) for (const sz of [-0.4, 0.4]) board(mat.darkWood, [0.1, 1.05, 0.1], [sx, 0.52, sz], g);
    const top = board(mat.wood, [1.7, 0.08, 1.0], [0, 1.1, 0], g);
    top.rotation.x = -0.2;
    const sheet = new THREE.Mesh(own(new THREE.PlaneGeometry(1.2, 0.6)), own(new THREE.MeshStandardMaterial({ map: texture(tex.writtenPapyrus(3)), roughness: 0.9 })));
    sheet.rotation.x = -Math.PI / 2 - 0.2;
    sheet.position.set(0, 1.155, 0);
    g.add(sheet);
    for (const sx of [-0.68, 0.68]) {
      const roller = new THREE.Mesh(own(new THREE.CylinderGeometry(0.06, 0.06, 0.66, 12)), mat.papyrus);
      roller.rotation.set(Math.PI / 2 - 0.2, 0, 0);
      roller.position.set(sx, 1.2, 0);
      roller.castShadow = true;
      g.add(roller);
    }
    const ink = lathe([[0, 0], [0.06, 0], [0.07, 0.05], [0.04, 0.09], [0, 0.09]], mat.darkBronze, 14);
    ink.position.set(0.62, 1.2, -0.42);
    g.add(ink);
    const pen = new THREE.Mesh(own(new THREE.CylinderGeometry(0.008, 0.008, 0.38, 6)), mat.wood);
    pen.rotation.set(0.2, 0, 1.0);
    pen.position.set(0.5, 1.26, -0.3);
    g.add(pen);
    board(mat.darkWood, [0.55, 0.06, 0.45], [0, 0.6, 0.95], g);
    for (const sx of [-0.2, 0.2]) board(mat.darkWood, [0.06, 0.6, 0.35], [sx, 0.3, 0.95], g);
    oilLamp(g, [-0.6, 1.2, -0.35]);
    g.rotation.y = -Math.PI / 2;
  }

  // Lint: the correctors' table under its lamps.
  {
    const g = station("lint", [2.6, 2.2, 2.2]);
    board(mat.wood, [2.3, 0.12, 1.3], [0, 0.95, 0], g);
    for (const sx of [-1.0, 1.0]) for (const sz of [-0.5, 0.5]) board(mat.darkWood, [0.12, 0.9, 0.12], [sx, 0.45, sz], g);
    for (let i = 0; i < 4; i++) {
      const s = new THREE.Mesh(own(new THREE.CylinderGeometry(0.07, 0.07, 0.6, 10)), mat.papyrus);
      s.rotation.z = Math.PI / 2;
      s.rotation.y = (i - 1.5) * 0.18;
      s.position.set(-0.5 + i * 0.12, 1.08, 0.25 - i * 0.12);
      s.castShadow = true;
      g.add(s);
    }
    const stand = lathe([[0.25, 0], [0.06, 0.08], [0.04, 1.9], [0.12, 2.0], [0.18, 2.06], [0, 2.08]], mat.bronze, 16);
    stand.position.set(0.85, 0, -0.85);
    g.add(stand);
    flame(g, [0.85, 2.2, -0.85], 0.6);
    oilLamp(g, [0.4, 1.01, -0.3]);
    oilLamp(g, [-0.3, 1.01, -0.35]);
    g.rotation.y = -Math.PI / 2;
  }

  // Graph: the armillary sphere before the middle window, its rings slowly turning.
  const armillary = new THREE.Group();
  {
    const g = station("graph", [3, 4.4, 3]);
    const plinth = lathe([[0.9, 0], [0.9, 0.2], [0.55, 0.3], [0.38, 0.4], [0.32, 1.6], [0.5, 1.75], [0.62, 1.85], [0, 1.85]], mat.marble, 28);
    g.add(plinth);
    armillary.position.set(0, 3.1, 0);
    g.add(armillary);
    const ring = (r: number, tube: number, m: THREE.Material) => {
      const t = new THREE.Mesh(own(new THREE.TorusGeometry(r, tube, 10, 72)), m);
      t.castShadow = true;
      return t;
    };
    const horizon = ring(1.25, 0.05, mat.bronze);
    horizon.rotation.x = Math.PI / 2;
    armillary.add(horizon);
    const meridian = ring(1.2, 0.045, mat.bronze);
    armillary.add(meridian);
    const inner = new THREE.Group();
    inner.name = "inner";
    armillary.add(inner);
    const equator = ring(1.05, 0.04, mat.bronze);
    equator.rotation.x = Math.PI / 2;
    inner.add(equator);
    const ecliptic = ring(1.0, 0.06, mat.darkBronze);
    ecliptic.rotation.x = Math.PI / 2 + (23.4 * Math.PI) / 180;
    inner.add(ecliptic);
    const colure = ring(0.98, 0.03, mat.bronze);
    colure.rotation.y = Math.PI / 2;
    inner.add(colure);
    const globe = new THREE.Mesh(own(new THREE.SphereGeometry(0.3, 32, 20)), mat.lapis);
    armillary.add(globe);
    const axis = new THREE.Mesh(own(new THREE.CylinderGeometry(0.02, 0.02, 2.7, 8)), mat.bronze);
    axis.rotation.z = (23.4 * Math.PI) / 180;
    armillary.add(axis);
    const fork = new THREE.Mesh(own(new THREE.CylinderGeometry(0.06, 0.1, 0.5, 12)), mat.bronze);
    fork.position.set(0, 1.95, 0);
    g.add(fork);
  }

  // Acquisition: a bronze ship model by the harbour window.
  {
    const g = station("acquisition", [1.6, 2.4, 1.6]);
    const ped = lathe([[0.5, 0], [0.5, 0.15], [0.3, 0.25], [0.26, 1.2], [0.45, 1.3], [0, 1.3]], mat.marble);
    g.add(ped);
    const ship = new THREE.Group();
    ship.position.set(0, 1.42, 0);
    const hull = new THREE.Mesh(own(new THREE.SphereGeometry(0.35, 20, 10, 0, Math.PI * 2, Math.PI / 2, Math.PI / 2)), mat.bronze);
    hull.scale.set(1.6, 0.6, 0.5);
    ship.add(hull);
    const mast = new THREE.Mesh(own(new THREE.CylinderGeometry(0.015, 0.015, 0.8, 6)), mat.darkBronze);
    mast.position.y = 0.4;
    ship.add(mast);
    const sail = new THREE.Mesh(own(new THREE.PlaneGeometry(0.5, 0.42)), mat.sail);
    sail.position.set(0, 0.45, 0.02);
    sail.rotation.y = Math.PI / 2;
    ship.add(sail);
    ship.rotation.y = 0.6;
    g.add(ship);
  }

  // Settings: Ctesibius' water clock, a vessel dripping into a column whose float points at the
  // hours.
  const drops: THREE.Mesh[] = [];
  {
    const g = station("settings", [1.8, 4, 1.8]);
    board(mat.marble, [1.4, 0.3, 1.0], [0, 0.15, 0], g);
    const tank = lathe([[0, 0], [0.32, 0.02], [0.4, 0.25], [0.36, 0.5], [0.4, 0.55], [0, 0.55]], mat.bronze);
    tank.position.set(-0.35, 2.4, 0);
    g.add(tank);
    board(mat.darkBronze, [0.06, 2.1, 0.06], [-0.35, 1.35, 0], g);
    const column = new THREE.Mesh(own(new THREE.CylinderGeometry(0.22, 0.22, 1.8, 24, 1, true)), mat.water);
    column.position.set(0.3, 1.2, 0);
    g.add(column);
    const water = new THREE.Mesh(own(new THREE.CylinderGeometry(0.2, 0.2, 0.9, 24)), own(new THREE.MeshStandardMaterial({ color: 0x3f7fa8, transparent: true, opacity: 0.7, roughness: 0.1 })));
    water.position.set(0.3, 0.75, 0);
    g.add(water);
    const float = new THREE.Mesh(own(new THREE.CylinderGeometry(0.17, 0.17, 0.08, 20)), mat.bronze);
    float.position.set(0.3, 1.24, 0);
    g.add(float);
    const pointer = new THREE.Mesh(own(new THREE.CylinderGeometry(0.012, 0.012, 1.0, 6)), mat.bronze);
    pointer.position.set(0.3, 1.74, 0);
    g.add(pointer);
    const scale = board(mat.marble, [0.12, 2.2, 0.12], [0.75, 1.4, 0], g);
    for (let i = 0; i < 12; i++) board(mat.darkBronze, [0.14, 0.015, 0.03], [0.75, 0.45 + i * 0.16, 0.06], g, false);
    scale.castShadow = true;
    const spout = new THREE.Mesh(own(new THREE.CylinderGeometry(0.02, 0.02, 0.35, 6)), mat.bronze);
    spout.rotation.z = Math.PI / 2.6;
    spout.position.set(-0.1, 2.48, 0);
    g.add(spout);
    for (let i = 0; i < 4; i++) {
      const d = new THREE.Mesh(own(new THREE.SphereGeometry(0.025, 8, 6)), mat.water);
      d.position.set(0.12, 2.3 - i * 0.25, 0);
      g.add(d);
      drops.push(d);
    }
    g.rotation.y = -0.4;
  }

  // ---- Furnishings --------------------------------------------------------------------------
  const frescoMat = own(new THREE.MeshStandardMaterial({ map: texture(tex.fresco(), [length / 9, 1]), roughness: 0.85 }));
  for (const side of [-1, 1]) {
    const f = new THREE.Mesh(own(new THREE.PlaneGeometry(length, 4.3)), frescoMat);
    f.rotation.y = side * -Math.PI / 2;
    f.position.set(side * (HALL.x - 0.03), 9.55, midZ);
    f.receiveShadow = true;
    room.add(f);
  }
  const amphoraGeo = own(
    new THREE.LatheGeometry(
      ([[0, 0], [0.09, 0.02], [0.13, 0.1], [0.22, 0.32], [0.24, 0.48], [0.2, 0.62], [0.1, 0.72], [0.07, 0.86], [0.1, 0.9], [0, 0.9]] as const).map(([r, y]) => new THREE.Vector2(r, y)),
      24,
    ),
  );
  const handleGeo = own(new THREE.TorusGeometry(0.09, 0.018, 6, 12, Math.PI));
  for (const side of [-1, 1]) {
    cabinetZ.forEach((z, i) => {
      const x = side * (HALL.x - 0.6);
      const top = 0.5 + rows * cellH + 0.24;
      for (const dz of i % 2 === 0 ? [-1.3, 1.3] : [0]) {
        const a = new THREE.Mesh(amphoraGeo, mat.terracotta);
        a.position.set(x, top, z + dz);
        a.rotation.y = Math.random() * Math.PI;
        a.castShadow = true;
        room.add(a);
        for (const hs of [-1, 1]) {
          const h = new THREE.Mesh(handleGeo, mat.terracotta);
          h.position.set(x, top + 0.7, z + dz + hs * 0.1);
          h.rotation.set(0, Math.PI / 2, hs * Math.PI / 2);
          room.add(h);
        }
      }
      if (i % 2 === 1) {
        for (const dz of [-1.4, 1.4]) {
          const head = new THREE.Mesh(own(new THREE.SphereGeometry(0.15, 16, 12)), mat.marble);
          head.scale.set(0.9, 1.15, 1);
          head.position.set(x, top + 0.62, z + dz);
          const shoulders = new THREE.Mesh(own(new THREE.SphereGeometry(0.24, 16, 8, 0, Math.PI * 2, 0, Math.PI / 2)), mat.marble);
          shoulders.scale.set(1, 0.9, 0.7);
          shoulders.position.set(x, top + 0.3, z + dz);
          const herm = new THREE.Mesh(own(new THREE.BoxGeometry(0.22, 0.3, 0.22)), mat.marble);
          herm.position.set(x, top + 0.15, z + dz);
          for (const m of [head, shoulders, herm]) {
            m.castShadow = true;
            room.add(m);
          }
        }
      }
    });
  }
  for (const [x, z] of [[-5.6, 2.6], [5.6, 2.6], [-5.6, -9.2], [5.6, -9.2]] as const) {
    const stand = lathe([[0.34, 0], [0.38, 0.05], [0.14, 0.18], [0.07, 0.36], [0.05, 2.3], [0.09, 2.38], [0.2, 2.46], [0.27, 2.56], [0.24, 2.6], [0, 2.58]], mat.bronze, 20);
    stand.position.set(x, 0, z);
    room.add(stand);
    for (let k = 0; k < 3; k++) {
      const foot = new THREE.Mesh(own(new THREE.SphereGeometry(0.07, 8, 6)), mat.bronze);
      const a = (k / 3) * Math.PI * 2;
      foot.position.set(x + Math.cos(a) * 0.32, 0.04, z + Math.sin(a) * 0.32);
      room.add(foot);
    }
    flame(room, [x, 2.82, z], 0.6);
  }
  // Velvet hangings at the arches, tied back toward the walls.
  for (const cx of ARCHES) {
    for (const sideOf of [-1, 1]) {
      // Two hangings meet on each pier between arches.
      const geo = own(new THREE.PlaneGeometry(1.0, 9.4, 30, 40));
      const pos = geo.getAttribute("position");
      const outer = sideOf * 0.5;
      for (let i = 0; i < pos.count; i++) {
        const x = pos.getX(i);
        const y = pos.getY(i);
        const worldY = y + 5.4;
        // Gathered to a third at the tie, a little wider below it, full at the rod.
        const tie = Math.exp(-((worldY - 3.4) ** 2) / 1.4);
        const below = worldY < 3.4 ? 0.25 : 0;
        const k = 1 - tie * 0.66 - below * (1 - tie);
        const nx = outer + (x - outer) * k;
        const z = Math.sin((x + 0.5) * Math.PI * 8) * 0.06 * (0.5 + tie);
        pos.setXYZ(i, nx, y, z);
      }
      geo.computeVertexNormals();
      const c = new THREE.Mesh(geo, mat.velvet);
      c.position.set(cx + sideOf * (ARCH.half + 0.5), 5.4, HALL.far + 0.55);
      c.castShadow = true;
      c.receiveShadow = true;
      room.add(c);
      const rod = new THREE.Mesh(own(new THREE.CylinderGeometry(0.03, 0.03, 1.1, 8)), mat.bronze);
      rod.rotation.z = Math.PI / 2;
      rod.position.set(c.position.x, 10.15, HALL.far + 0.5);
      room.add(rod);
    }
  }

  // ---- Lamps hanging over the aisle ---------------------------------------------------------
  const lampLights: THREE.PointLight[] = [];
  for (const [x, z] of [[-3.6, 2], [3.6, 2], [-3.6, -9], [3.6, -9]] as const) {
    const chain = new THREE.Mesh(own(new THREE.CylinderGeometry(0.012, 0.012, 3.4, 4)), mat.darkBronze);
    chain.position.set(x, HALL.height - 1.7, z);
    room.add(chain);
    const bowl = lathe([[0, 0], [0.24, 0.04], [0.3, 0.13], [0.27, 0.16], [0, 0.15]], mat.bronze);
    bowl.position.set(x, HALL.height - 3.55, z);
    room.add(bowl);
    flame(room, [x, HALL.height - 3.3, z], 0.65);
    const light = new THREE.PointLight(0xffa552, 0, 20, 2);
    light.position.set(x, HALL.height - 3.4, z);
    room.add(light);
    lampLights.push(light);
  }

  // ---- Outside: the terrace, the sea and the sky --------------------------------------------
  const outside = new THREE.Group();
  scene.add(outside);
  board(mat.stoneOut, [40, 12, 9], [0, -6, HALL.far - 5.6], outside);
  for (let x = -19; x <= 19; x += 0.9) {
    const post = lathe([[0.12, 0], [0.16, 0.2], [0.09, 0.5], [0.15, 0.85], [0.12, 1.0], [0, 1.0]], mat.stoneOut, 12);
    post.position.set(x, 0, HALL.far - 9.6);
    outside.add(post);
  }
  board(mat.stoneOut, [40, 0.16, 0.5], [0, 1.08, HALL.far - 9.6], outside);
  const sun = new THREE.Vector3(0, 0.3, -1).normalize();
  const cloudTex = texture(tex.cloud());
  const skyUniforms = {
    zenith: { value: new THREE.Color() },
    horizon: { value: new THREE.Color() },
    sunColor: { value: new THREE.Color() },
    sunDir: { value: sun.clone() },
    sunDisk: { value: 1 },
    sunGlow: { value: 1 },
  };
  const skyDome = new THREE.Mesh(
    own(new THREE.SphereGeometry(1800, 48, 24)),
    own(new THREE.ShaderMaterial({ uniforms: skyUniforms, vertexShader: SKY_VERTEX, fragmentShader: SKY_FRAGMENT, side: THREE.BackSide, depthWrite: false })),
  );
  skyDome.renderOrder = -1;
  scene.add(skyDome);
  const pmrem = new THREE.PMREMGenerator(renderer);
  const envScene = new THREE.Scene();
  envScene.add(new THREE.Mesh(skyDome.geometry, skyDome.material));
  const envGround = new THREE.Mesh(own(new THREE.CircleGeometry(1500, 32)), own(new THREE.MeshBasicMaterial({ color: 0x2c2118 })));
  envGround.rotation.x = -Math.PI / 2;
  envGround.position.y = -40;
  envScene.add(envGround);
  let envTarget: THREE.WebGLRenderTarget | null = null;
  const reflective: [THREE.MeshStandardMaterial, number][] = [
    [mat.bronze, 1.1],
    [mat.darkBronze, 0.9],
    [mat.marble, 0.35],
    [mat.column, 0.25],
    [floorMat, 0.3],
    [mat.water, 1],
    [mat.lapis, 0.6],
    [mat.seal, 0.4],
    [mat.terracotta, 0.25],
    [mat.stoneOut, 0.7],
    [mat.sail, 0.6],
  ];
  function updateEnvironment() {
    if (soft) return;
    const next = pmrem.fromScene(envScene, 0.02);
    for (const [m, k] of reflective) {
      const first = m.envMap === null;
      m.envMap = next.texture;
      m.envMapIntensity = k;
      if (first) m.needsUpdate = true;
    }
    envTarget?.dispose();
    envTarget = next;
  }

  // The sea of clouds the library stands in, and puffs rising past the terrace's edge.
  const cloudSea = {
    top: { value: new THREE.Color() },
    shade: { value: new THREE.Color() },
    horizon: { value: new THREE.Color() },
    sunColor: { value: new THREE.Color() },
    sunDir: { value: sun.clone() },
    sunLight: { value: 1 },
    time: { value: 0 },
  };
  const below = new THREE.Mesh(
    own(new THREE.PlaneGeometry(5000, 3000)),
    own(new THREE.ShaderMaterial({ uniforms: cloudSea, vertexShader: CLOUDS_VERTEX, fragmentShader: CLOUDS_FRAGMENT })),
  );
  below.rotation.x = -Math.PI / 2;
  below.position.set(0, -2.5, -1500);
  scene.add(below);
  const puffs: THREE.Sprite[] = [];
  {
    const pr = tex.seeded(17);
    const heaps = [texture(tex.cumulus(81)), texture(tex.cumulus(82)), texture(tex.cumulus(83))];
    const puff = (x: number, y: number, z: number, w: number, h: number) => {
      const map = heaps[Math.floor(pr() * heaps.length)] ?? cloudTex;
      const sp = new THREE.Sprite(own(new THREE.SpriteMaterial({ map, transparent: true, depthWrite: false, opacity: 0 })));
      sp.position.set(x, y, z);
      sp.scale.set(w, h, 1);
      scene.add(sp);
      puffs.push(sp);
    };
    // The cloud tops around the library, from just past the railing to the horizon: each band
    // farther and larger, so they fill the lower part of every arch. Farthest first, so nearer
    // heaps draw over them.
    const bands: [number, number, number, number, number][] = [
      // [count, nearest z offset, depth, width, top height]
      [16, 700, 500, 420, 70],
      [18, 320, 300, 230, 34],
      [16, 140, 160, 120, 16],
      [12, 40, 80, 50, 6],
    ];
    for (const [count, near, depth, width, top] of bands) {
      for (let i = 0; i < count; i++) {
        const z = HALL.far - near - pr() * depth;
        const w = width * (0.7 + pr() * 0.6);
        const spread = (HALL.far - z) * 0.9;
        puff(-spread + pr() * spread * 2, -6 + pr() * top * 0.4 + w * 0.12, z, w, w * 0.5);
      }
    }
    puffs.sort((a, b) => a.position.z - b.position.z);
    puffs.forEach((p, i) => (p.renderOrder = i));
  }

  // Stars and the moon.
  const starGeo = own(new THREE.BufferGeometry());
  {
    const pts: number[] = [];
    const sr = tex.seeded(5);
    for (let i = 0; i < 2600; i++) {
      const y = 0.03 + sr() * 0.97;
      const a = sr() * Math.PI * 2;
      const r = Math.sqrt(1 - y * y);
      pts.push(Math.cos(a) * r * 1700, y * 1700, Math.sin(a) * r * 1700);
    }
    starGeo.setAttribute("position", new THREE.Float32BufferAttribute(pts, 3));
  }
  const starMat = own(new THREE.PointsMaterial({ color: 0xfdf6e8, size: 1.6, sizeAttenuation: false, transparent: true, opacity: 0, depthWrite: false }));
  const stars = new THREE.Points(starGeo, starMat);
  scene.add(stars);
  const moonTex = texture(tex.glow(256, "rgba(255,255,250,1)", "rgba(220,230,255,0.25)"));
  const moon = new THREE.Sprite(own(new THREE.SpriteMaterial({ map: moonTex, color: 0xe8eeff, transparent: true, depthWrite: false, blending: THREE.AdditiveBlending })));
  moon.scale.setScalar(90);
  scene.add(moon);

  // Clouds.
  const clouds: THREE.Sprite[] = [];
  {
    const cr = tex.seeded(13);
    for (let i = 0; i < 22; i++) {
      const s = new THREE.Sprite(own(new THREE.SpriteMaterial({ map: cloudTex, transparent: true, depthWrite: false, opacity: 0 })));
      s.position.set(-900 + cr() * 1800, 60 + cr() * 260, -500 - cr() * 900);
      const k = 320 + cr() * 420;
      s.scale.set(k, k * 0.3, 1);
      scene.add(s);
      clouds.push(s);
    }
  }

  // Rain beyond the arches.
  const rainCount = 1600;
  const rainGeo = own(new THREE.BufferGeometry());
  const rainPos = new Float32Array(rainCount * 6);
  {
    const rr = tex.seeded(23);
    for (let i = 0; i < rainCount; i++) {
      const x = -24 + rr() * 48;
      const y = -3 + rr() * 26;
      const z = HALL.far - 2 - rr() * 40;
      rainPos.set([x, y, z, x + 0.05, y - 0.7, z], i * 6);
    }
    rainGeo.setAttribute("position", new THREE.BufferAttribute(rainPos, 3));
  }
  const rainMat = own(new THREE.LineBasicMaterial({ color: 0xc8d2dc, transparent: true, opacity: 0 }));
  const rain = new THREE.LineSegments(rainGeo, rainMat);
  scene.add(rain);

  // Shafts of light through the arches, and dust turning in them.
  const shaftTex = texture(tex.shaft());
  const shafts: THREE.Mesh[] = [];
  for (const cx of ARCHES) {
    const geo = own(new THREE.PlaneGeometry(ARCH.half * 2, 26));
    geo.translate(0, -13, 0);
    const m = new THREE.Mesh(geo, own(new THREE.MeshBasicMaterial({ map: shaftTex, transparent: true, opacity: 0, blending: THREE.AdditiveBlending, depthWrite: false, side: THREE.DoubleSide })));
    m.position.set(cx, ARCH.spring + 1.5, HALL.far + 0.2);
    scene.add(m);
    shafts.push(m);
  }
  const dustGeo = own(new THREE.BufferGeometry());
  {
    const dp: number[] = [];
    const dr = tex.seeded(31);
    for (let i = 0; i < 500; i++) dp.push(-11 + dr() * 22, 0.5 + dr() * 9, HALL.far + 1 - dr() * -16);
    dustGeo.setAttribute("position", new THREE.Float32BufferAttribute(dp, 3));
  }
  const dustMat = own(new THREE.PointsMaterial({ color: 0xffe6b8, size: 0.035, transparent: true, opacity: 0, depthWrite: false, blending: THREE.AdditiveBlending }));
  const dust = new THREE.Points(dustGeo, dustMat);
  scene.add(dust);

  // ---- Light --------------------------------------------------------------------------------
  const hemi = new THREE.HemisphereLight(0xffffff, 0x5a4632, 1);
  scene.add(hemi);
  const sunLight = new THREE.DirectionalLight(0xffffff, 1);
  sunLight.castShadow = !soft;
  sunLight.shadow.mapSize.set(2048, 2048);
  const sc = sunLight.shadow.camera;
  sc.left = -26;
  sc.right = 26;
  sc.top = 30;
  sc.bottom = -30;
  sc.near = 1;
  sc.far = 220;
  sunLight.shadow.bias = -0.0004;
  sunLight.shadow.normalBias = 0.03;
  const sunTarget = new THREE.Object3D();
  sunTarget.position.set(0, 0, -6);
  scene.add(sunTarget);
  sunLight.target = sunTarget;
  scene.add(sunLight);
  // A warm fill inside, so the far end isn't black when the sun is high.
  const fill = new THREE.PointLight(0xffe2b8, 0, 40, 1.2);
  fill.position.set(0, 9, -20);
  scene.add(fill);

  // ---- Post-processing: bloom on the flames and the sun; the finish (a software renderer gets
  // none of it) ------------------------------------------------------------------------------
  const composer = soft ? null : new EffectComposer(renderer);
  const bloom = soft ? null : new UnrealBloomPass(new THREE.Vector2(512, 512), 0.5, 0.55, 1.25);
  const finish = soft ? null : new ShaderPass(FINISH);
  if (composer && bloom && finish) {
    composer.addPass(new RenderPass(scene, camera));
    composer.addPass(bloom);
    composer.addPass(new OutputPass());
    composer.addPass(finish);
  }

  let current: Sky | null = null;
  const lampTarget = { value: 0 };

  function setSky(s: Sky) {
    current = s;
    const e = (s.elevation * Math.PI) / 180;
    // Low suns rise in the left arch and set in the right one.
    sun.set(s.across * 0.24, Math.sin(e), -Math.cos(e)).normalize();
    skyUniforms.zenith.value.setHex(s.zenith);
    skyUniforms.horizon.value.setHex(s.horizon);
    skyUniforms.sunColor.value.setHex(s.sun);
    skyUniforms.sunDir.value.copy(sun);
    const isNight = s.stars > 0.5;
    skyUniforms.sunDisk.value = isNight ? 0 : 1 - s.rain;
    skyUniforms.sunGlow.value = isNight ? 0.15 : 1 - s.rain * 0.9;
    cloudSea.top.value.setHex(s.cloudColor).lerp(new THREE.Color(s.horizon), 0.25);
    cloudSea.shade.value.setHex(s.below);
    cloudSea.horizon.value.setHex(s.horizon);
    cloudSea.sunColor.value.setHex(isNight ? 0xc8d4ff : s.sun);
    cloudSea.sunDir.value.copy(sun);
    cloudSea.sunLight.value = isNight ? 0.3 : s.sunLight * (1 - s.rain * 0.8);
    for (const pf of puffs) {
      // A touch brighter than the sky's own light, so the cloud tops stand out against it.
      pf.material.color.setHex(s.cloudColor).multiplyScalar(isNight ? 1 : 1.25 - s.rain * 0.3);
      pf.material.opacity = 0.95;
    }
    hemi.color.setHex(s.ambientColor);
    hemi.groundColor.setHex(0x5a4632).lerp(new THREE.Color(s.ambientColor), 0.25);
    hemi.intensity = s.ambient * (isNight ? 0.7 : 1.25);
    sunLight.color.setHex(s.sun);
    sunLight.intensity = s.sunLight * (isNight ? 0.6 : 1.6);
    sunLight.position.copy(sunTarget.position).addScaledVector(sun, 120);
    fill.intensity = isNight ? 0 : (1 - s.lamps) * 18;
    fill.color.setHex(s.sun);
    lampTarget.value = s.lamps;
    starMat.opacity = s.stars * (1 - s.rain);
    moon.material.opacity = Math.max(0, s.stars - 0.3) * 1.2;
    moon.position.copy(sun).multiplyScalar(1500);
    for (const c of clouds) {
      c.material.color.setHex(s.cloudColor);
      c.material.opacity = Math.min(1, s.clouds * (0.55 + s.rain * 0.45));
    }
    rainMat.opacity = s.rain * 0.38;
    rain.visible = s.rain > 0.01;
    const shaftOn = s.shafts * (1 - s.rain) * (s.elevation < 40 ? 1 : 0.5);
    for (const m of shafts) {
      (m.material as THREE.MeshBasicMaterial).opacity = shaftOn * 0.13;
      (m.material as THREE.MeshBasicMaterial).color.setHex(s.sun);
      const into = sun.clone().negate();
      m.quaternion.setFromUnitVectors(new THREE.Vector3(0, -1, 0), into);
    }
    dustMat.opacity = shaftOn * 0.55;
    for (const f of flames) f.sprite.material.opacity = 0.25 + s.lamps * 0.75;
    renderer.toneMappingExposure = isNight ? 1.0 : s.rain > 0.5 ? 1.1 : 1.05;
    if (bloom) bloom.strength = isNight ? 0.75 : s.lamps > 0.4 ? 0.55 : 0.35;
    updateEnvironment();
    requestRender();
  }

  // ---- Camera walks -------------------------------------------------------------------------
  let width_ = 1;
  let height_ = 1;
  let panel = 0;
  const from = { pos: HOME.position.clone(), look: HOME.target.clone(), panel: 0 };
  const to = { pos: HOME.position.clone(), look: HOME.target.clone(), panel: 0 };
  let walkStart = -1;
  const WALK = 1400;
  let focused: Station | null = null;

  function focus(s: Station | null, panelPx: number) {
    focused = s;
    const pose = s ? PLACES[s].pose : HOME;
    from.pos.copy(camera.position);
    from.look.copy(lookAt);
    from.panel = panel;
    to.pos.copy(pose.position);
    to.look.copy(pose.target);
    to.panel = panelPx;
    if (!motion) {
      camera.position.copy(to.pos);
      lookAt.copy(to.look);
      panel = to.panel;
      walkStart = -1;
      applyView();
      requestRender();
    } else walkStart = performance.now();
  }

  function applyView() {
    camera.lookAt(lookAt);
    const w = width_;
    const h = height_;
    if (panel > 1) {
      camera.aspect = (w + panel) / h;
      camera.setViewOffset(w + panel, h, panel, 0, w, h);
    } else {
      camera.aspect = w / h;
      camera.clearViewOffset();
    }
    camera.updateProjectionMatrix();
  }

  function resize() {
    const r = canvas.getBoundingClientRect();
    width_ = Math.max(1, Math.round(r.width));
    height_ = Math.max(1, Math.round(r.height));
    renderer.setSize(width_, height_, false);
    if (composer) {
      composer.setPixelRatio(renderer.getPixelRatio());
      composer.setSize(width_, height_);
    }
    applyView();
    requestRender();
  }
  const observer = new ResizeObserver(resize);
  observer.observe(canvas);

  // ---- Picking ------------------------------------------------------------------------------
  const ray = new THREE.Raycaster();
  const ndc = new THREE.Vector2();
  let hovered: Station | null = null;
  let highlighted: Station | null = null;
  function stationAt(ev: PointerEvent): Station | null {
    const r = canvas.getBoundingClientRect();
    ndc.set(((ev.clientX - r.left) / r.width) * 2 - 1, -((ev.clientY - r.top) / r.height) * 2 + 1);
    ray.setFromCamera(ndc, camera);
    const hit = ray.intersectObjects(picks, false)[0];
    const id: unknown = hit?.object.userData.station;
    return typeof id === "string" && isStation(id) ? id : null;
  }
  const onMove = (ev: PointerEvent) => {
    const s = stationAt(ev);
    if (s !== hovered) {
      hovered = s;
      canvas.style.cursor = s ? "pointer" : "";
      opts.onHover(s);
      requestRender();
    }
  };
  let downAt: [number, number] | null = null;
  const onDown = (ev: PointerEvent) => {
    downAt = [ev.clientX, ev.clientY];
  };
  const onUp = (ev: PointerEvent) => {
    if (!downAt) return;
    const moved = Math.hypot(ev.clientX - downAt[0], ev.clientY - downAt[1]);
    downAt = null;
    if (moved > 6) return;
    const s = stationAt(ev);
    if (s) opts.onPick(s);
  };
  // The GPU went away (a driver reset, waking from sleep): the list layout takes over.
  const onLost = (e: Event) => {
    e.preventDefault();
    opts.onLost();
  };
  canvas.addEventListener("webglcontextlost", onLost);
  canvas.addEventListener("pointermove", onMove);
  canvas.addEventListener("pointerdown", onDown);
  canvas.addEventListener("pointerup", onUp);

  // ---- The loop -----------------------------------------------------------------------------
  let frameCb: ((a: Anchor[]) => void) | null = null;
  const anchorPoint = new THREE.Vector3();
  function anchors(): Anchor[] {
    const out: Anchor[] = [];
    for (const [id, p] of Object.entries(PLACES) as [Station, (typeof PLACES)[Station]][]) {
      anchorPoint.copy(p.label).project(camera);
      const inFront = anchorPoint.z < 1;
      const x = ((anchorPoint.x + 1) / 2) * width_;
      const y = ((1 - anchorPoint.y) / 2) * height_;
      out.push({ station: id, x, y, visible: inFront && x > -40 && x < width_ + 40 && y > -40 && y < height_ + 40 });
    }
    return out;
  }

  let needsRender = true;
  let covered = false;
  // While a page is open beside the hall and the camera is still, about twenty frames a second
  // are enough for the flames and clouds.
  let lastDrawn = 0;
  const OPEN_FRAME_MS = 50;
  function requestRender() {
    needsRender = true;
  }
  const timer = new THREE.Timer();
  let lampLevel = 0;
  let elapsed = 0;
  function frame(time: number) {
    timer.update(time);
    const dt = Math.min(timer.getDelta(), 0.1);
    const animate = motion && !document.hidden;
    if (animate) elapsed += dt;
    // The walk.
    if (walkStart >= 0) {
      const t = Math.min(1, (performance.now() - walkStart) / WALK);
      const k = t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
      camera.position.lerpVectors(from.pos, to.pos, k);
      lookAt.lerpVectors(from.look, to.look, k);
      panel = from.panel + (to.panel - from.panel) * k;
      if (t >= 1) walkStart = -1;
      applyView();
      needsRender = true;
    } else if (animate && focused === null) {
      // Standing at the entrance, breathing.
      camera.position.set(HOME.position.x + Math.sin(elapsed * 0.13) * 0.25, HOME.position.y + Math.sin(elapsed * 0.21) * 0.06, HOME.position.z);
      applyView();
      needsRender = true;
    }
    // Lamps come up and down gently with the light.
    lampLevel += (lampTarget.value - lampLevel) * Math.min(1, dt * 2 + (motion ? 0 : 1));
    lampLights.forEach((l, i) => {
      const flicker = animate ? 1 + Math.sin(elapsed * 9 + i * 1.7) * 0.05 + Math.sin(elapsed * 23 + i) * 0.03 : 1;
      l.intensity = lampLevel * 70 * flicker;
    });
    if (animate) {
      for (const f of flames) f.sprite.scale.setScalar(f.base * (1 + Math.sin(elapsed * 11 + f.phase) * 0.08 + Math.sin(elapsed * 27 + f.phase) * 0.04));
      armillary.rotation.y += dt * 0.08;
      const inner = armillary.getObjectByName("inner");
      if (inner) inner.rotation.y -= dt * 0.17;
      cloudSea.time.value = elapsed;
      for (const pf of puffs) {
        pf.position.x += dt * 1.6;
        if (pf.position.x > 300) pf.position.x = -300;
      }
      for (const c of clouds) {
        c.position.x += dt * 3;
        if (c.position.x > 1000) c.position.x = -1000;
      }
      if (rain.visible) {
        const p = rainGeo.getAttribute("position");
        if (p instanceof THREE.BufferAttribute) {
          for (let i = 0; i < rainCount; i++) {
            let y = rainPos[i * 6 + 1] ?? 0;
            y -= dt * 22;
            if (y < -3) y += 26;
            rainPos[i * 6 + 1] = y;
            rainPos[i * 6 + 4] = y - 0.7;
          }
          p.needsUpdate = true;
        }
      }
      dust.rotation.y = Math.sin(elapsed * 0.05) * 0.05;
      drops.forEach((d, i) => {
        d.position.y = 2.3 - ((elapsed * 0.5 + i * 0.25) % 1) * 1.0;
      });
      needsRender = true;
    }
    // The hovered or highlighted station's ring.
    for (const [id, ring] of rings) {
      const m = ring.material as THREE.MeshBasicMaterial;
      const want = id === hovered || id === highlighted ? 0.55 : 0;
      if (Math.abs(m.opacity - want) > 0.01) {
        m.opacity += (want - m.opacity) * Math.min(1, dt * 8 + (motion ? 0 : 1));
        needsRender = true;
      }
    }
    const idleOpen = focused !== null && walkStart < 0 && time - lastDrawn < OPEN_FRAME_MS;
    if (needsRender && current && !covered && !idleOpen) {
      lastDrawn = time;
      if (composer && finish) {
        const { time, center } = finish.uniforms;
        if (time) time.value = elapsed;
        if (center) center.value = (width_ - panel) / 2 / width_;
        composer.render(dt);
      } else renderer.render(scene, camera);
      needsRender = false;
      frameCb?.(anchors());
    }
  }
  renderer.setAnimationLoop(frame);
  resize();

  return {
    setSky,
    focus,
    onFrame(cb) {
      frameCb = cb;
      requestRender();
    },
    highlight(s) {
      highlighted = s;
      requestRender();
    },
    cover(c) {
      covered = c;
      requestRender();
    },
    dispose() {
      renderer.setAnimationLoop(null);
      observer.disconnect();
      canvas.removeEventListener("webglcontextlost", onLost);
      canvas.removeEventListener("pointermove", onMove);
      canvas.removeEventListener("pointerdown", onDown);
      canvas.removeEventListener("pointerup", onUp);
      scene.traverse((o) => {
        if (o instanceof THREE.Mesh || o instanceof THREE.Points || o instanceof THREE.LineSegments || o instanceof THREE.Sprite) {
          o.geometry.dispose();
        }
      });
      for (const d of disposables) d.dispose();
      composer?.dispose();
      envTarget?.dispose();
      pmrem.dispose();
      renderer.dispose();
    },
  };
}
