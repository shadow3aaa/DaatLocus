import { searchMessages, type MessageRecord, type Page } from "./actor-api";
import { messageDisplayText } from "./conversation-import";

export function matchesConversationText(text: string, query: string) {
  return text.toLowerCase().includes(query.trim().toLowerCase());
}

export function matchingConversationMessages(items: MessageRecord[], live: MessageRecord[], query: string) {
  const messages = new Map(items.map(message => [message.id, message]));
  for (const message of live) messages.set(message.id, message);
  return [...messages.values()].filter(message => matchesConversationText(messageDisplayText(message), query) || matchesConversationText(message.reply || "", query)).sort((a, b) => a.seq - b.seq);
}

type Search = (actor: string, query: string, options: { before?: number; signal: AbortSignal }) => Promise<Page<MessageRecord>>;
type SearchState = { query: string; items: MessageRecord[]; next: number | null; pending: boolean; failed: boolean };

export class ConversationSearch {
  private state: SearchState = { query: "", items: [], next: null, pending: false, failed: false };
  private listeners = new Set<() => void>();
  private controller = new AbortController();
  private epoch = 0;
  private paging = false;
  constructor(private actor: string, private request: Search = searchMessages) {}
  getSnapshot = () => this.state;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  private publish(state: SearchState) { this.state = state; this.listeners.forEach(listener => listener()); }
  cancel = () => { this.epoch++; this.controller.abort(); this.paging = false; };
  async search(query: string) {
    this.cancel();
    const epoch = this.epoch;
    this.controller = new AbortController();
    const signal = this.controller.signal;
    this.publish({query, items: [], next: null, pending: Boolean(query), failed: false});
    if (!query) return;
    try {
      const page = await this.request(this.actor, query, {signal});
      if (epoch === this.epoch) this.publish({query, items: page.items, next: page.next_cursor, pending: false, failed: false});
    } catch {
      if (epoch === this.epoch && !signal.aborted) this.publish({...this.state, pending: false, failed: true});
    }
  }
  async more(signal: AbortSignal) {
    if (this.paging || this.controller.signal.aborted || this.state.pending || this.state.next === null) return;
    const epoch = this.epoch;
    const before = this.state.next;
    this.paging = true;
    try {
      const page = await this.request(this.actor, this.state.query, {before, signal: AbortSignal.any([signal, this.controller.signal])});
      if (epoch !== this.epoch || signal.aborted) return;
      const items = new Map([...page.items, ...this.state.items].map(message => [message.id, message]));
      this.publish({...this.state, items: [...items.values()].sort((a,b) => a.seq-b.seq), next: page.next_cursor});
    } finally { if (epoch === this.epoch) this.paging = false; }
  }
}
