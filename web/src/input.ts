import type { ClientMessage, KnobState, TouchContact } from './protocol';

const MAX_CONTACTS = 2;
const KNOB_FLICK = 127;

interface ActivePointer {
  slot: number;
  x: number;
  y: number;
  down: boolean;
}

const emptyKnob = (overrides: Partial<KnobState> = {}): KnobState => ({
  select: false,
  home: false,
  back: false,
  x: 0,
  y: 0,
  wheel: 0,
  ...overrides,
});

/** Maps browser pointer/keyboard input to CarPlay touch and knob HID messages. */
export class InputController {
  private readonly pointers = new Map<number, ActivePointer>();
  private pendingFlush = 0;
  private enabled = false;
  private attached = false;

  constructor(
    private readonly surface: HTMLElement,
    private readonly send: (message: ClientMessage) => void,
  ) {}

  setEnabled(enabled:boolean):void{if(this.enabled===enabled)return;if(!enabled)this.reset();this.enabled=enabled;this.surface.inert=!enabled;}
  reset():void{if(this.pendingFlush){window.cancelAnimationFrame(this.pendingFlush);this.pendingFlush=0;}const ids=[...this.pointers.keys()];this.pointers.clear();for(const id of ids){try{if(this.surface.hasPointerCapture(id))this.surface.releasePointerCapture(id);}catch{}}this.pointers.clear();this.send({type:'touch',contacts:[]});}
  attach(): void {
    if(this.attached)return;this.attached=true;
    window.addEventListener("blur",this.cancel);window.addEventListener("resize",this.cancel);
    const surface = this.surface;
    surface.style.touchAction = 'none';
    surface.addEventListener('pointerdown', this.onPointerDown, { passive: false });
    surface.addEventListener('pointermove', this.onPointerMove, { passive: false });
    surface.addEventListener('pointerup', this.onPointerUp, { passive: false });
    surface.addEventListener('pointercancel', this.onPointerUp, { passive: false });
    surface.addEventListener('lostpointercapture', this.onPointerUp, { passive: false });
    window.addEventListener('keydown', this.onKeyDown);
  }

  detach(): void {
    this.setEnabled(false);this.attached=false;
    window.removeEventListener("blur",this.cancel);window.removeEventListener("resize",this.cancel);
    const surface = this.surface;
    surface.removeEventListener('pointerdown', this.onPointerDown);
    surface.removeEventListener('pointermove', this.onPointerMove);
    surface.removeEventListener('pointerup', this.onPointerUp);
    surface.removeEventListener('pointercancel', this.onPointerUp);
    surface.removeEventListener('lostpointercapture', this.onPointerUp);
    window.removeEventListener('keydown', this.onKeyDown);
  }

  private readonly onPointerDown = (event: PointerEvent): void => {
    if(!this.enabled)return;
    const point=this.point(event.clientX,event.clientY);if(!point)return;
    event.preventDefault();
    if (this.pointers.size >= MAX_CONTACTS) return;
    this.surface.focus({ preventScroll: true });
    this.surface.setPointerCapture(event.pointerId);
    this.pointers.set(event.pointerId, {
      slot: this.nextSlot(),
      x: point.x,
      y: point.y,
      down: true,
    });
    this.scheduleFlush();
  };

  private readonly onPointerMove = (event: PointerEvent): void => {
    const pointer = this.pointers.get(event.pointerId);
    if (!pointer) return;
    event.preventDefault();
    const point=this.point(event.clientX,event.clientY);
    if(!point){this.onPointerUp(event);return;}
    pointer.x = point.x;
    pointer.y = point.y;
    this.scheduleFlush();
  };

  private readonly onPointerUp = (event: PointerEvent): void => {
    const pointer = this.pointers.get(event.pointerId);
    if (!pointer) return;
    event.preventDefault();
    const point = this.point(event.clientX, event.clientY);
    if (event.type === 'pointerup' && point) {
      // Deliver a pending press/move before lifting at the same location. An
      // empty report jumps the released finger to (0, 0), cancelling a tap.
      if (this.pendingFlush) this.flush();
      pointer.x = point.x;
      pointer.y = point.y;
      pointer.down = false;
      this.flush();
    }
    this.pointers.delete(event.pointerId);
    if(this.surface.hasPointerCapture(event.pointerId))this.surface.releasePointerCapture(event.pointerId);
    if (event.type !== 'pointerup' || !point) this.flush();
  };

  private readonly onKeyDown = (event: KeyboardEvent): void => {
    if (!this.enabled || this.surface.inert || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    if (document.querySelector('dialog[open]')) return;
    if (event.target instanceof Element && (event.target.closest('input, select, textarea, [contenteditable="true"]') || ((event.key === 'Enter' || event.key === ' ') && event.target.closest('button, a, summary')))) return;
    const knob = (overrides: Partial<KnobState>) => {
      event.preventDefault();
      this.send({ type: 'knob', knob: emptyKnob(overrides) });
    };
    switch (event.key) {
      case 'ArrowUp':
        knob({ y: -KNOB_FLICK });
        break;
      case 'ArrowDown':
        knob({ y: KNOB_FLICK });
        break;
      case 'ArrowLeft':
        knob({ x: -KNOB_FLICK });
        break;
      case 'ArrowRight':
        knob({ x: KNOB_FLICK });
        break;
      case 'Enter':
        knob({ select: true });
        break;
      case 'Escape':
      case 'Backspace':
        knob({ back: true });
        break;
      case 'Home':
        knob({ home: true });
        break;
      case 's':
        event.preventDefault();
        this.send({ type: 'siri' });
        break;
      case ' ':
        event.preventDefault();
        this.send({ type: 'media', media: 3 });
        break;
      case 'MediaPlayPause':
        event.preventDefault();
        this.send({ type: 'media', media: 3 });
        break;
      case 'MediaTrackNext':
        event.preventDefault();
        this.send({ type: 'media', media: 4 });
        break;
      case 'MediaTrackPrevious':
        event.preventDefault();
        this.send({ type: 'media', media: 5 });
        break;
      default:
        break;
    }
  };

  private nextSlot(): number {
    const used = new Set(Array.from(this.pointers.values(), (pointer) => pointer.slot));
    for (let slot = 0; slot < MAX_CONTACTS; slot += 1) {
      if (!used.has(slot)) return slot;
    }
    return 0;
  }

  private scheduleFlush(): void {
    if (this.pendingFlush !== 0) return;
    this.pendingFlush = window.requestAnimationFrame(() => this.flush());
  }

  private flush(): void {
    if (this.pendingFlush) window.cancelAnimationFrame(this.pendingFlush);
    this.pendingFlush = 0;
    const contacts: TouchContact[] = Array.from(this.pointers.values()).map((pointer) => ({
      id: pointer.slot,
      x: pointer.x,
      y: pointer.y,
      down: pointer.down,
    }));
    this.send({ type: 'touch', contacts });
  }

  private readonly cancel=():void=>this.reset();
  private point(x:number,y:number):{x:number;y:number}|null {
    const r=this.surface.getBoundingClientRect();
    const c=this.surface as HTMLCanvasElement;
    const scale=Math.min(r.width/(c.width||r.width),r.height/(c.height||r.height));
    const w=(c.width||r.width)*scale,h=(c.height||r.height)*scale;
    const left=r.left+(r.width-w)/2,top=r.top+(r.height-h)/2;
    if(w<=0||h<=0||x<left||x>left+w||y<top||y>top+h)return null;
    return {x:(x-left)/w,y:(y-top)/h};
  }
}
