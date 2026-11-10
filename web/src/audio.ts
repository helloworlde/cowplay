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
}

const MIN_LATENCY_SECONDS = 0.12;
const MAX_LATENCY_SECONDS = 0.35;

/** Decodes and plays CarPlay audio streams (AAC-LC, Opus, LPCM) via Web Audio. */
export class AudioEngine {
  private readonly context: AudioContext;
  private readonly gain: GainNode;
  private readonly streams = new Map<number, StreamState>();
  private readonly streamGains = new Map<string, number>();
  private unlocked = false;

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
    };
    if (message.codec === AudioCodec.AacLc || message.codec === AudioCodec.Opus) {
      if (typeof AudioDecoder === 'undefined') {
        this.onError(
          'WebCodecs audio decoding needs a secure context. Open the https:// URL printed by the server.',
        );
        this.streams.set(message.streamType, state);
        return;
      }
      let decoder: AudioDecoder;
      try {
        decoder = new AudioDecoder({
          output: (data) => {
            try {
              this.playAudioData(state, data);
            } finally {
              data.close();
            }
          },
          error: (error) => this.onError(`Audio decoder error: ${error.message}`),
        });
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
        decoder.configure(config);
        state.decoder = decoder;
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
      if (state.decoder.decodeQueueSize >= 16) {this.onError('Audio decoder overloaded; stream stopped');this.closeStream(message.streamType);return;}
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
    if (state.sources.size>=64 || state.nextPlayTime>now+MAX_LATENCY_SECONDS) {
      for (const source of state.sources) {try {source.onended=null;source.stop();}catch{} source.disconnect();}
      state.sources.clear();state.nextPlayTime=0;this.onError('Audio backlog reset; playback discontinuity');
    }
    // Preserve queued continuity while packets arrive within the remaining buffer.
    if (state.nextPlayTime <= now || state.nextPlayTime > now + MAX_LATENCY_SECONDS) {
      state.nextPlayTime = now + MIN_LATENCY_SECONDS;
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
