// Car browser microphone → iPhone input stream (PCM only).
// The server announces {type:'mic', state:'start', rate} when the iPhone opens a telephony or
// Siri input stream; we capture at that rate and send 20 ms PCM16LE frames as [8, ...pcm].
export class MicUplink {
  private stream: MediaStream | null = null;
  private context: AudioContext | null = null;
  private node: ScriptProcessorNode | null = null;
  private generation = 0;
  private pending = new Int16Array(0);
  constructor(private readonly send: (frame: ArrayBuffer) => void, private readonly report: (text: string) => void) {}
  get active(): boolean { return this.stream !== null; }

  async start(rate: number): Promise<void> {
    this.stop();
    const generation = ++this.generation;
    if (!navigator.mediaDevices?.getUserMedia) { this.report('车机浏览器不支持麦克风接口'); return; }
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({audio: {channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true}});
    } catch (error) {
      this.report(`麦克风未授权或不可用：${(error as Error).name}`);
      return;
    }
    if (generation !== this.generation) { stream.getTracks().forEach(t => t.stop()); return; }
    // Asking the context for the phone's rate makes the browser resample the capture.
    const context = new AudioContext({sampleRate: rate});
    const source = context.createMediaStreamSource(stream);
    const node = context.createScriptProcessor(1024, 1, 1);
    const frame = Math.round(rate / 50);
    node.onaudioprocess = event => {
      const input = event.inputBuffer.getChannelData(0);
      const merged = new Int16Array(this.pending.length + input.length);
      merged.set(this.pending);
      for (let i = 0; i < input.length; i++) merged[this.pending.length + i] = Math.max(-32768, Math.min(32767, Math.round(input[i] * 32767)));
      let offset = 0;
      for (; offset + frame <= merged.length; offset += frame) {
        const out = new Uint8Array(1 + frame * 2);
        out[0] = 8;
        const view = new DataView(out.buffer, 1);
        for (let i = 0; i < frame; i++) view.setInt16(i * 2, merged[offset + i], true);
        this.send(out.buffer);
      }
      this.pending = merged.slice(offset);
    };
    source.connect(node);
    node.connect(context.destination); // ScriptProcessor only runs while connected; output is silent
    this.stream = stream; this.context = context; this.node = node;
    this.report(`麦克风已开启（${rate} Hz）`);
  }

  stop(): void {
    this.generation++;
    this.node?.disconnect();
    this.stream?.getTracks().forEach(t => t.stop());
    void this.context?.close().catch(() => {});
    if (this.stream) this.report('麦克风已关闭');
    this.stream = null; this.context = null; this.node = null; this.pending = new Int16Array(0);
  }
}
