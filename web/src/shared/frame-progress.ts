/** Silence alone says nothing about a static picture. Time only pending drawing. */
export class FrameProgress {
  private paint = 0;
  private pending: number | null = null;
  reset(): void { this.paint = 0; this.pending = null; }
  blockedSince(arrival: number, paint: number, now: number, limit: number): number | null {
    if (paint !== this.paint) { this.paint = paint; this.pending = null; }
    if (arrival <= paint) { this.pending = null; return null; }
    // Start with new input, never with the age of a picture shown before an idle.
    this.pending ??= Math.min(arrival, now);
    return now - this.pending >= limit ? this.pending : null;
  }
}
