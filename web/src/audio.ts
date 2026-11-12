import { AudioCodec, type AudioConfigMessage, type AudioPacketMessage } from './protocol';

interface StreamState {
  kind:number;
  lastFeedback:number;
  codec: number;
  sampleRate: number;
  channels: number;
  name: string;
  decoder: AudioDecoder | null;
  nextPlayTime: number;
  gain: GainNode;
  sources: Set<AudioBufferSourceNode>;
  active: boolean;
  minLatency: number;
  maxLatency: number;
}

const MIN_LATENCY_SECONDS = 0.12;
const MAX_LATENCY_SECONDS = 0.35;
// Music (MainHighAudio, type 102) shares the WebSocket with large video frames; a deeper
// jitter buffer rides out head-of-line delays where prompts and touch sounds stay short.
const MEDIA_MIN_LATENCY_SECONDS = 0.25;
const MEDIA_MAX_LATENCY_SECONDS = 0.75;
// About 1.4 s of AAC (1024 frames at 48 kHz); a burst larger than this is dropped, not fatal.
const MAX_DECODE_QUEUE = 64;

/** Decodes and plays CarPlay audio streams (AAC-LC, Opus, LPCM) via Web Audio. */
export class AudioEngine {
  private readonly context: AudioContext;
  private readonly gain: GainNode;
  private readonly streams = new Map<number, StreamState>();
  private readonly streamGains = new Map<string, number>();
  private unlocked = false;
  /** Underruns and dropped late packets since the page loaded, for the viewer metrics. */
  readonly counters = {underruns: 0, drops: 0};

  constructor(private readonly onError: (message: string) => void,private readonly onProgress:(kind:number,timestampUs:number)=>void=()=>{}) {
    this.context = new AudioContext({ latencyHint: 'interactive' });
    this.gain = this.context.createGain();
    this.gain.connect(this.context.destination);
  }

  get isUnlocked(): boolean {
    return this.unlocked;
  }

  /** Must run inside a user gesture so the browser allows audio playback. */
  async unlock(): Promise<void> {
    await this.context.resume();
    this.unlocked = true;
  }

  setVolume(volume: number): void {
    this.gain.gain.value = Math.min(1, Math.max(0, volume));
  }

  setStreamGain(name: string, value: number): void {
    const gain = Math.min(1, Math.max(0, value));
    this.streamGains.set(name, gain);
    for (const state of this.streams.values()) {
      if (state.name === name) state.gain.gain.value = gain;
    }
  }

  /** Milliseconds of audio already scheduled ahead of playback, across the active streams. */
  bufferedMs(): number | undefined {
    const now = this.context.currentTime;
    let ahead: number | undefined;
    for (const state of this.streams.values()) if (state.nextPlayTime > now) ahead = Math.max(ahead ?? 0, (state.nextPlayTime - now) * 1000);
    return ahead === undefined ? undefined : Math.round(ahead);
  }

  activeStreamNames(): string[] {
    return Array.from(new Set(Array.from(this.streams.values(), (state) => state.name))).sort();
  }

  /** Last accepted media stream format since all streams were torn down, for the debug overlay. */
  info: {rate: number; channels: number; codec: string} | null = null;
  handleConfig(message: AudioConfigMessage): void {
    if ([1,2,3].includes(message.codec)) this.info = {rate: message.sampleRate, channels: message.channels, codec: ({1: 'aac', 2: 'opus', 3: 'lpcm'} as Record<number,string>)[message.codec] + (message.name ? ` · ${message.name}` : '')};
    this.closeStream(message.streamType);
    if (![100,101,102].includes(message.streamType) || ![1,2,3].includes(message.codec) || ![1,2].includes(message.channels) || message.sampleRate<8000 || message.sampleRate>48000) {this.onError('Unsupported audio configuration');return;}
    const streamGain = this.context.createGain();
    streamGain.gain.value = this.streamGains.get(message.name) ?? 1;
    streamGain.connect(this.gain);
    const state: StreamState = {
      kind:message.streamType,
      lastFeedback:0,
      codec: message.codec,
      sampleRate: message.sampleRate,
      channels: message.channels,
      name: message.name,
      decoder: null,
      nextPlayTime: 0,
      gain: streamGain,
      sources: new Set(),
      active: true,
      minLatency: message.streamType === 102 ? MEDIA_MIN_LATENCY_SECONDS : MIN_LATENCY_SECONDS,
      maxLatency: message.streamType === 102 ? MEDIA_MAX_LATENCY_SECONDS : MAX_LATENCY_SECONDS,
    };
    if (message.codec === AudioCodec.AacLc || message.codec === AudioCodec.Opus) {
      if (typeof AudioDecoder === 'undefined') {
        this.onError(
          'WebCodecs audio decoding needs a secure context. Open the https:// URL printed by the server.',
        );
        this.streams.set(message.streamType, state);
        return;
      }
      try {
        const config: AudioDecoderConfig =
          message.codec === AudioCodec.AacLc
            ? {
                codec: 'mp4a.40.2',
                sampleRate: message.sampleRate,
                numberOfChannels: message.channels,
                description: message.description.length > 0 ? message.description : undefined,
              }
            : {
                codec: 'opus',
                sampleRate: message.sampleRate,
                numberOfChannels: message.channels,
              };
        let recovered = false;
        const createDecoder = (): AudioDecoder => {
          const decoder = new AudioDecoder({
            output: (data) => {
              try {
                this.playAudioData(state, data);
              } finally {
                data.close();
              }
            },
            error: (error) => {
              if (!state.active || state.decoder !== decoder) return;
              if (!recovered) {
                recovered = true;
                try {
                  state.decoder = createDecoder();
                  this.onError(`音频解码异常，已尝试恢复：${error.message}`);
                  return;
                } catch { /* Report the original failure and retain the reconnect path. */ }
              }
              this.onError(`音频解码已停止，请停止后重新连接：${error.message}`);
            },
          });
          try { decoder.configure(config); } catch (error) { decoder.close(); throw error; }
          return decoder;
        };
        state.decoder = createDecoder();
      } catch (error) {
        this.onError(`Audio decoder rejected ${message.codec === AudioCodec.AacLc ? 'AAC' : 'Opus'}: ${describeError(error)}`);
      }
    }
    this.streams.set(message.streamType, state);
  }

  handlePacket(message: AudioPacketMessage): void {
    const state = this.streams.get(message.streamType);
    if (!state) return;
    if (state.decoder) {
      if (state.decoder.state === 'closed') return;
      // Queued audio arrives in bursts. Bound the decoder by dropping this packet only; closing
      // the stream here left the page silent for the rest of the session.
      if (state.decoder.decodeQueueSize >= MAX_DECODE_QUEUE) {this.counters.drops++;return;}
      try {
        state.decoder.decode(
          new EncodedAudioChunk({
            type: 'key',
            timestamp: message.timestampUs,
            data: message.data,
          }),
        );
      } catch (error) {
        this.onError(`Audio decode failed: ${describeError(error)}`);
      }
      return;
    }
    if (state.codec === AudioCodec.Lpcm) {
      this.playPcm(state, message.data,message.timestampUs);
    }
  }

  stopStream(streamType: number): void {
    this.closeStream(streamType);
  }

  stopAll(): void {
    for (const streamType of this.streams.keys()) this.closeStream(streamType);
    this.info = null;
  }

  private closeStream(streamType: number): void {
    const state = this.streams.get(streamType);
    if (!state) return;
    this.streams.delete(streamType);
    state.active=false;
    for (const source of state.sources) {try {source.onended=null;source.stop();}catch{} source.disconnect();}
    state.sources.clear();
    runCatchingDisconnect(state.gain);
    if (state.decoder) {
      try {
        state.decoder.close();
      } catch {
        // Already closed.
      }
    }
  }

  private playAudioData(state: StreamState, data: AudioData): void {
    if (!this.unlocked || !state.active) return;
    const numberOfChannels = Math.max(1, data.numberOfChannels);
    const buffer = this.context.createBuffer(numberOfChannels, data.numberOfFrames, data.sampleRate);
    for (let channel = 0; channel < numberOfChannels; channel += 1) {
      const plane = new Float32Array(data.numberOfFrames);
      data.copyTo(plane, { planeIndex: channel, format: 'f32-planar' });
      buffer.copyToChannel(plane, channel);
    }
    this.schedule(state, buffer,data.timestamp);
  }

  private playPcm(state: StreamState, payload: Uint8Array,timestampUs:number): void {
    if (!this.unlocked || !state.active || payload.length < 2 || payload.length % (2*state.channels)!==0) return;
    const sampleCount = Math.floor(payload.length / 2);
    const channels = Math.max(1, state.channels);
    const frames = Math.max(1, Math.floor(sampleCount / channels));
    const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
    const buffer = this.context.createBuffer(channels, frames, state.sampleRate);
    for (let channel = 0; channel < channels; channel += 1) {
      const plane = new Float32Array(frames);
      for (let frame = 0; frame < frames; frame += 1) {
        const offset = (frame * channels + channel) * 2;
        plane[frame] = view.getInt16(offset, true) / 32768;
      }
      buffer.copyToChannel(plane, channel);
    }
    this.schedule(state, buffer,timestampUs);
  }

  private schedule(state: StreamState, buffer: AudioBuffer,timestampUs:number): void {
    if (!state.active) return;
    const now = this.context.currentTime;
    // Too far ahead: drop only this late burst. Stopping the queued sources here used to cut
    // the audio that was already playing, which is what the listener heard as stutter.
    if (state.sources.size>=64 || state.nextPlayTime>now+state.maxLatency) {
      this.counters.drops++;
      return;
    }
    // Preserve queued continuity while packets arrive within the remaining buffer.
    if (state.nextPlayTime <= now) {
      if(state.nextPlayTime>0)this.counters.underruns++;
      state.nextPlayTime = now + state.minLatency;
    }
    const source = this.context.createBufferSource();
    source.buffer = buffer;
    source.connect(state.gain);
    state.sources.add(source);
    source.onended=()=>{state.sources.delete(source);source.disconnect();const now=performance.now();if(state.active&&this.context.state==='running'&&now-state.lastFeedback>=250){state.lastFeedback=now;this.onProgress(state.kind,Math.round(timestampUs+buffer.duration*1e6));}};
    source.start(state.nextPlayTime);
    state.nextPlayTime += buffer.duration;
  }
}

function describeError(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

function runCatchingDisconnect(node: AudioNode): void {
  try {
    node.disconnect();
  } catch {
    // Already disconnected.
  }
}
