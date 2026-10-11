// Whether this browser can draw the hall (WebGL), asked without loading three.js.

let answer: boolean | null = null;

export function canDraw(): boolean {
  if (answer !== null) return answer;
  try {
    const c = document.createElement("canvas");
    const gl = c.getContext("webgl2") ?? c.getContext("webgl");
    answer = gl !== null;
    gl?.getExtension("WEBGL_lose_context")?.loseContext();
  } catch {
    answer = false;
  }
  return answer;
}
