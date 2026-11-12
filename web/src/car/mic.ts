// Browser PCM/Opus microphone uplink. Opus framing follows the locked PlayPort MicrophoneUplink.
export interface MicRequest {
  rate: number;
  channels: number;
  codec: 'lpcm' | 'opus';
  samplesPerPacket: number;
  bitrate: number | null;
  epoch: number;
}

export class MicUplink {
  private stream: MediaStream | null = null;
  private context: AudioContext | null = null;
  private node: ScriptProcessorNode | null = null;
  private encoder: AudioEncoder | null = null;
  private generation = 0;
  private pending = new Float32Array(0);
  constructor(private readonly send: (frame: ArrayBuffer) => void, private readonly report: (text: string) => void) {}
  get active(): boolean { return this.stream !== null; }

  async start(request: MicRequest): Promise<void> {
    this.stop();
    const generation = ++this.generation;
    const current = () => generation === this.generation;
    try {
      const {rate, channels, codec, samplesPerPacket, epoch} = request;
      if (!Number.isSafeInteger(epoch) || epoch < 1 || channels !== 1 ||
          !['lpcm','opus'].includes(codec) || samplesPerPacket !== rate / 50 ||
          (codec === 'opus' ? rate !== 48000 : ![8000,16000,24000,32000,44100,48000].includes(rate))) {
        throw new Error('不支持的麦克风格式');
      }
      const config: AudioEncoderConfig = {codec:'opus',sampleRate:rate,numberOfChannels:1,
        bitrate:request.bitrate ?? 32000,opus:{format:'opus',frameDuration:20000}};
      if (codec === 'opus') {
        if (typeof AudioEncoder === 'undefined' || typeof AudioData === 'undefined' ||
            !(await AudioEncoder.isConfigSupported(config)).supported) {
          throw new Error('此浏览器不支持 Opus 麦克风编码');
        }
        if (!current()) return;
      }
      if (!navigator.mediaDevices?.getUserMedia) throw new Error('此浏览器不支持麦克风接口');
      const stream = await navigator.mediaDevices.getUserMedia({audio: {
        channelCount:1,echoCancellation:true,noiseSuppression:true,autoGainControl:true,
      }});
      if (!current()) { stream.getTracks().forEach(t => t.stop()); return; }
      this.stream = stream;
      const context = new AudioContext({sampleRate:rate});
      this.context = context;
      await context.resume();
      if (!current()) return;

      const packet = (tag:number,payload:Uint8Array) => {
        if (!current()) return;
        const out = new Uint8Array(9 + payload.length);
        out[0] = tag;
        new DataView(out.buffer).setBigUint64(1,BigInt(epoch),true);
        out.set(payload,9); this.send(out.buffer);
      };
      let timestamp = 0;
      if (codec === 'opus') {
        const encoder = new AudioEncoder({
          output: chunk => {
            if (!current()) return;
            const payload = new Uint8Array(chunk.byteLength); chunk.copyTo(payload);
            packet(9,payload);
          },
          error: error => { if (current()) { this.stop(); this.report(`麦克风编码失败：${error.message}`); } },
        });
        this.encoder = encoder;
        encoder.configure(config);
      }
      const source = context.createMediaStreamSource(stream);
      const node = context.createScriptProcessor(1024,1,1);
      this.node = node;
      node.onaudioprocess = event => {
        if (!current()) return;
        try {
          const input = event.inputBuffer.getChannelData(0);
          const merged = new Float32Array(this.pending.length + input.length);
          merged.set(this.pending); merged.set(input,this.pending.length);
          let offset = 0;
          for (; offset + samplesPerPacket <= merged.length; offset += samplesPerPacket) {
            const samples = merged.subarray(offset,offset + samplesPerPacket);
            if (this.encoder) {
              if (this.encoder.encodeQueueSize >= 16) throw new Error('Opus 编码积压，麦克风已停止');
              const data = new AudioData({format:'f32-planar',sampleRate:rate,numberOfChannels:1,
                numberOfFrames:samples.length,timestamp,data:new Float32Array(samples)});
              timestamp += 20000;
              try { this.encoder.encode(data); } finally { data.close(); }
            } else {
              const pcm = new Uint8Array(samples.length * 2), view = new DataView(pcm.buffer);
              for (let i=0;i<samples.length;i++) view.setInt16(i*2,
                Math.max(-32768,Math.min(32767,Math.round(samples[i]*32767))),true);
              packet(8,pcm);
            }
          }
          this.pending = merged.slice(offset);
        } catch (error) {
          if (current()) { this.stop(); this.report(`麦克风编码失败：${(error as Error).message}`); }
        }
      };
      source.connect(node);
      node.connect(context.destination); // ScriptProcessor output remains silent.
      this.report(`麦克风已开启（${rate} Hz · ${codec}）`);
    } catch (error) {
      if (current()) { this.stop(); this.report(`麦克风未授权或不可用：${(error as Error).message}`); }
    }
  }

  stop(): void {
    this.generation++;
    if (this.node) this.node.onaudioprocess = null;
    this.node?.disconnect();
    try { this.encoder?.close(); } catch { /* May already be closed after an encoder error. */ }
    this.stream?.getTracks().forEach(t => t.stop());
    void this.context?.close().catch(() => {});
    if (this.stream) this.report('麦克风已关闭');
    this.stream = null; this.context = null; this.node = null; this.encoder = null;
    this.pending = new Float32Array(0);
  }
}
