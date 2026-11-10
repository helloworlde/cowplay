import { annexBToAvcc, avcCodecString, VideoCodec, type VideoConfigMessage, type VideoFrameMessage } from './protocol';

interface DecoderState {
  decoder: VideoDecoder;
  usesDescription: boolean;
  waitingKeyframe: boolean;
  width: number;
  height: number;
}

interface HevcCandidate {
  codec: string;
  description?: Uint8Array;
  software: boolean;
}

const STALL_TIMEOUT_MS = 2500;

/** Decodes CarPlay's H.264/H.265 stream with WebCodecs and paints it to a canvas. */
export class VideoPlayer {
  private state: DecoderState | null = null;
  private lastConfig: VideoConfigMessage | null = null;
  private preferHardware = true;
  private generation = 0;
  private watchdog: number | null = null;
  private framesRendered = 0;
  private lastFrameSize = { width: 0, height: 0 };
  private submitted = new Map<number, number>();
  private decodeSamples: number[] = [];
  private paintTimes: number[] = [];
  private decodedTotal = 0;
  private droppedTotal = 0;
  private lastPaintAt = 0;
  /** Codec string of the active decoder and colour space of the last painted frame. */
  codecName: string | null = null;
  color: string | null = null;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly onError: (message: string) => void,
    private readonly onFrame: () => void,
    private readonly onRequestKeyframe: () => void,
    private readonly onResolutionChange: (width: number, height: number) => void = () => {},
  ) {}

  get isConfigured(): boolean {
    return this.state !== null;
  }

  get decodeQueueSize(): number {
    return this.state?.decoder.decodeQueueSize ?? 0;
  }

  /** Timestamp of the most recent frame arrival; used for the latency readout. */
  lastArrivalAt = 0;

  configure(message: VideoConfigMessage): void {
    // Keyframe recovery may replay the same config. Keep an active decoder so
    // that a slow first frame is not repeatedly cancelled by its watchdog.
    const last = this.lastConfig;
    if (this.state?.decoder.state === 'configured' && last &&
        last.codec === message.codec && last.streamType === message.streamType &&
        last.width === message.width && last.height === message.height &&
        last.codecData.length === message.codecData.length &&
        last.codecData.every((byte, index) => byte === message.codecData[index])) return;
    void this.configureAsync(message);
  }

  private async configureAsync(message: VideoConfigMessage): Promise<void> {
    this.close();
    const generation = ++this.generation;
    this.lastConfig = message;
    if (typeof VideoDecoder === 'undefined') {
      this.onError(
        'WebCodecs video decoding needs a secure context. Open the https:// URL printed by the server ' +
          'with a browser-trusted certificate. Do not bypass certificate warnings.',
      );
      return;
    }
    try {
      if (message.codec === VideoCodec.H264) {
        this.configureH264(message, generation);
        return;
      }
      await this.configureH265(message, generation);
    } catch (error) {
      if (generation === this.generation) {
        this.fail(`Video decoder rejected the stream: ${describeError(error)}`);
      }
    }
  }

  private configureH264(message: VideoConfigMessage, generation: number): void {
    const descriptionPresent = message.codecData.length > 4;
    const config: VideoDecoderConfig = {
      codec: (this.codecName = avcCodecString(message.codecData)),
      codedWidth: message.width,
      codedHeight: message.height,
      hardwareAcceleration: this.preferHardware ? 'prefer-hardware' : 'no-preference',
      ...(descriptionPresent ? { description: message.codecData } : {}),
    };
    const decoder = this.createDecoder();
    decoder.configure(config);
    if (generation !== this.generation) {
      decoder.close();
      return;
    }
    this.adopt(decoder, descriptionPresent, message);
  }

  private async configureH265(message: VideoConfigMessage, generation: number): Promise<void> {
    const candidates = hevcCodecCandidates(message.codecData);
    for (const candidate of candidates) {
      if (generation !== this.generation) return;
      const config: VideoDecoderConfig = {
        codec: candidate.codec,
        codedWidth: message.width,
        codedHeight: message.height,
        hardwareAcceleration: candidate.software ? 'no-preference' : 'prefer-hardware',
        ...(candidate.description ? { description: candidate.description } : {}),
      };
      const support = await VideoDecoder.isConfigSupported(config).catch(() => null);
      if (generation !== this.generation) return;
      if (!support?.supported) continue;
      const decoder = this.createDecoder();
      try {
        decoder.configure(config);
      } catch (error) {
        decoder.close();
        continue;
      }
      this.codecName = candidate.codec;
      this.adopt(decoder, candidate.description !== undefined, message);
      return;
    }
    this.onError(
      'This phone sent H.265/HEVC, but this browser cannot decode it. ' +
        'Switch the video codec back to H.264 in the Display panel.',
    );
    this.onRequestKeyframe();
  }

  private adopt(decoder: VideoDecoder, usesDescription: boolean, message: VideoConfigMessage): void {
    this.lastFrameSize = { width: 0, height: 0 };
    this.state = {
      decoder,
      usesDescription,
      waitingKeyframe: true,
      width: message.width,
      height: message.height,
    };
    this.resizeCanvas(message.width, message.height);
    this.framesRendered = 0;
    this.armWatchdog();
    this.onError('');
  }

  pushFrame(message: VideoFrameMessage): void {
    const state = this.state;
    if (!state) return;
    this.lastArrivalAt = performance.now();
    const data = state.usesDescription ? annexBToAvcc(message.data) : message.data;
    if (data.length === 0) return;
    if (state.decoder.decodeQueueSize > 8) {
      if (!state.waitingKeyframe) this.onRequestKeyframe();
      state.waitingKeyframe = true;
      this.droppedTotal++;
      return;
    }
    if (state.waitingKeyframe && !message.keyframe) {this.droppedTotal++;return;}
    if (!Number.isSafeInteger(message.timestampUs) || message.timestampUs < 0) return;
    state.waitingKeyframe = false;
    if (this.submitted.size > 64) this.submitted.clear();
    this.submitted.set(message.timestampUs, performance.now());
    try {
      state.decoder.decode(
        new EncodedVideoChunk({
          type: message.keyframe ? 'key' : 'delta',
          timestamp: message.timestampUs,
          data,
        }),
      );
    } catch (error) {
      this.fail(`Video decode failed: ${describeError(error)}`);
    }
  }

  reset():void{this.close();this.lastConfig=null;this.lastArrivalAt=0;this.decodeSamples=[];this.paintTimes=[];this.submitted.clear();this.lastPaintAt=0;}

  close(): void {
    this.canvas.getContext('2d')?.clearRect(0,0,this.canvas.width,this.canvas.height);
    this.framesRendered=0;
    this.lastFrameSize = { width: 0, height: 0 };
    this.generation++;
    const state = this.state;
    this.state = null;
    this.clearWatchdog();
    if (state) {
      try {
        state.decoder.close();
      } catch {
        // Already closed.
      }
    }
  }

  /**
   * Tears the decoder down and recovers: silently retries in software when the hardware decoder
   * failed, and asks the phone for a fresh keyframe (which also re-sends the codec config).
   */
  private fail(message: string): void {
    this.close();
    if (this.preferHardware) {
      this.preferHardware = false;
      if (this.lastConfig) {
        this.configure(this.lastConfig);
        return;
      }
    }
    this.onError(message);
    this.onRequestKeyframe();
  }

  private createDecoder(): VideoDecoder {
    const generation=this.generation;
    return new VideoDecoder({
      output: (frame) => {
        if(generation!==this.generation){frame.close();return;}
        try {
          this.paint(frame);
        } finally {
          frame.close();
        }
      },
      error: (error) => {if(generation===this.generation)this.fail(`Video decoder error: ${describeError(error)}`);},
    });
  }

  private armWatchdog(): void {
    this.clearWatchdog();
    this.watchdog = window.setTimeout(() => {
      this.watchdog = null;
      if (this.state && this.framesRendered === 0) {
        this.onRequestKeyframe();
        this.armWatchdog();
      }
    }, STALL_TIMEOUT_MS);
  }

  private clearWatchdog(): void {
    if (this.watchdog !== null) {
      window.clearTimeout(this.watchdog);
      this.watchdog = null;
    }
  }

  /** Browser-side measurements for the diagnostics overlay; no estimates. */
  stats(): {decoded:number;dropped:number;decodeMs:number|null;fps:number|null;intervals:number[];lastPaintAt:number;lastArrivalAt:number;width:number;height:number;decoder:string} {
    const now = performance.now();
    const recent = this.paintTimes.filter((t) => now - t <= 1000);
    const intervals: number[] = [];
    for (let i = 1; i < this.paintTimes.length; i++) intervals.push(this.paintTimes[i] - this.paintTimes[i - 1]);
    const d = this.decodeSamples;
    return {
      decoded: this.decodedTotal,
      dropped: this.droppedTotal,
      decodeMs: d.length ? d.reduce((a, b) => a + b, 0) / d.length : null,
      fps: this.paintTimes.length > 1 && now - this.lastPaintAt < 2000 ? recent.length : this.state ? 0 : null,
      intervals: intervals.slice(-60),
      lastPaintAt: this.lastPaintAt,
      lastArrivalAt: this.lastArrivalAt,
      width: this.lastFrameSize.width,
      height: this.lastFrameSize.height,
      decoder: this.state ? (this.preferHardware ? 'webcodecs 硬件优先' : 'webcodecs 软件') : '未配置',
    };
  }

  private paint(frame: VideoFrame): void {
    const cs = frame.colorSpace;
    if (cs?.primaries) this.color = `${cs.primaries} / ${cs.matrix ?? cs.primaries}`;
    this.framesRendered += 1;
    const now = performance.now();
    const submittedAt = this.submitted.get(frame.timestamp);
    if (submittedAt !== undefined) {
      this.submitted.delete(frame.timestamp);
      this.decodeSamples.push(now - submittedAt);
      if (this.decodeSamples.length > 60) this.decodeSamples.shift();
    }
    this.decodedTotal++;
    this.lastPaintAt = now;
    this.paintTimes.push(now);
    if (this.paintTimes.length > 61) this.paintTimes.shift();
    this.clearWatchdog();
    const context = this.canvas.getContext('2d');
    if (!context) return;
    if (frame.displayWidth !== this.lastFrameSize.width || frame.displayHeight !== this.lastFrameSize.height) {
      this.lastFrameSize = { width: frame.displayWidth, height: frame.displayHeight };
      this.resizeCanvas(frame.displayWidth, frame.displayHeight);
      this.onResolutionChange(frame.displayWidth, frame.displayHeight);
    }
    context.drawImage(frame, 0, 0, this.canvas.width, this.canvas.height);
    this.onFrame();
  }

  private resizeCanvas(width: number, height: number): void {
    if (width > 0 && height > 0) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
  }
}

/**
 * Builds HEVC codec strings from an hvcC record, most specific first.
 *
 * WebCodecs accepts `hvc1` with an hvcC description; the profile-compatibility flags in the codec
 * string are bit-reversed relative to the record (ISO/IEC 14496-15).
 */
function hevcCodecCandidates(hvcC: Uint8Array): HevcCandidate[] {
  const candidates: HevcCandidate[] = [];
  if (hvcC.length >= 13) {
    const view = new DataView(hvcC.buffer, hvcC.byteOffset, hvcC.byteLength);
    const profileSpace = ['', 'A', 'B', 'C'][(hvcC[1] >> 6) & 0x03] ?? '';
    const tier = (hvcC[1] & 0x20) !== 0 ? 'H' : 'L';
    const profileIdc = hvcC[1] & 0x1f;
    const compat = reverseBits32(view.getUint32(2)) >>> 0;
    const levelIdc = hvcC[12];
    const constraints: number[] = [];
    for (let index = 6; index <= 11; index += 1) constraints.push(hvcC[index]);
    while (constraints.length > 0 && constraints[constraints.length - 1] === 0) constraints.pop();
    const constraintHex = constraints
      .map((value) => value.toString(16).padStart(2, '0'))
      .join('')
      .toUpperCase();
    const base = `hvc1.${profileSpace}${profileIdc}.${compat.toString(16)}.${tier}${levelIdc}`;
    if (constraintHex) candidates.push({ codec: `${base}.${constraintHex}`, description: hvcC, software: false });
    candidates.push({ codec: base, description: hvcC, software: false });
    candidates.push({ codec: base, description: hvcC, software: true });
  }
  candidates.push({ codec: 'hvc1.1.6.L120.B0', description: hvcC, software: true });
  return candidates;
}

function reverseBits32(value: number): number {
  let result = 0;
  for (let index = 0; index < 32; index += 1) {
    result = ((result << 1) | (value & 1)) >>> 0;
    value >>>= 1;
  }
  return result >>> 0;
}

function describeError(error: unknown): string {
  if (error instanceof Error) return `${error.name}: ${error.message}`;
  return String(error);
}
