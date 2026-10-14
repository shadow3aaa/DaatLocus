export class AutoSave<T> {
  private queued: T | undefined;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private running: Promise<void> | undefined;
  private retained: T | undefined;
  constructor(private save: (value: T) => Promise<unknown>, private failed: (error: unknown) => void, private saved?: () => void, private delay = 450) {}
  queue(value: T) {
    this.queued = value;
    this.retained = undefined;
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => { this.timer = undefined; void this.flush(); }, this.delay);
  }
  flush(): Promise<void> {
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    if (this.running) return this.running;
    if(this.queued === undefined && this.retained !== undefined){this.queued=this.retained;this.retained=undefined;}
    this.running = (async () => {
      while (this.queued !== undefined) {
        const value = this.queued; this.queued = undefined;
        try { await this.save(value); this.saved?.(); }
        catch (error) { if(this.queued===undefined)this.retained=value;this.failed(error); }
      }
    })().finally(() => { this.running = undefined; if(this.queued!==undefined)return this.flush(); });
    return this.running;
  }
}
