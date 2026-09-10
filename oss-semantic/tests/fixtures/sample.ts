/** Shape of a point. */
export interface Point {
  x: number;
  y: number;
}

export function norm(p: Point): number {
  return Math.hypot(p.x, p.y);
}
