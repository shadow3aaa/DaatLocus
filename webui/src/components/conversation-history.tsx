import { messageDisplayText } from "@/lib/conversation-import";
import { useLayoutEffect, useState, type CSSProperties, type RefObject } from "react";
import ReactMarkdown from "react-markdown";
import { t } from "@/lib/i18n";
import { motionScrollBehavior } from "@/lib/appearance";
import { historyStops, historyViewport } from "@/lib/conversation-history";
import type { MessageRecord } from "@/lib/actor-api";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

const tickWidths = [26, 20, 14, 10];
const messageAnchor = (message: MessageRecord) => `message-${message.actor_id}-${message.id}`;

/** A local index of the loaded work history; Core never mounts this control. */
export function ConversationHistory({messages, scroll, onNavigate}: {
  messages: MessageRecord[]; scroll: RefObject<HTMLDivElement | null>; onNavigate(): void;
}) {
  const [position, setPosition] = useState({current: -1, visible: [] as number[], capacity: 0, opacity: 0});
  const [hovered, setHovered] = useState<number | null>(null);
  const [focused, setFocused] = useState<number | null>(null);
  const preview = hovered ?? focused;
  useLayoutEffect(() => {
    const area = scroll.current;
    const list = area?.firstElementChild;
    if (!area || !list) return;
    const controls = area.parentElement?.querySelector<HTMLElement>(".dl-conversation-footer");
    const composer = controls?.querySelector<HTMLElement>(".dl-composer");
    const heading = area.closest(".dl-actor-dialog")?.querySelector<HTMLElement>(".dl-actor-dialog-heading");
    const anchors = messages.map(message => list.querySelector<HTMLElement>(`#${CSS.escape(messageAnchor(message))}`));
    let frame = 0;
    const measure = () => {
      const paneStyle = getComputedStyle(area.parentElement!);
      const header = Number.parseFloat(paneStyle.getPropertyValue("--dl-header-space")) || 0;
      const footer = Number.parseFloat(paneStyle.getPropertyValue("--dl-footer-space")) || 0;
      const pitch = 10 * (Number.parseFloat(paneStyle.getPropertyValue("--ui-font-size")) || 14) / 14;
      const viewport = area.getBoundingClientRect();
      const scale = viewport.width / area.offsetWidth || 1;
      const proseInset = (list.getBoundingClientRect().left - viewport.left) / scale + Number.parseFloat(getComputedStyle(list).paddingLeft);
      const composerInset = composer ? (composer.getBoundingClientRect().left - viewport.left) / scale : proseInset;
      const required = (Number.parseFloat(paneStyle.getPropertyValue("--dl-history-left")) || 14) + (Number.parseFloat(paneStyle.getPropertyValue("--dl-history-width")) || 36) + (Number.parseFloat(paneStyle.getPropertyValue("--dl-history-gap")) || 12);
      const opacity = Number(Math.min(1, Math.max(0, (Math.min(proseInset, composerInset) - required) / 8)).toFixed(2));
      // The rail lives in the outer margin; its spacing stays fixed as drafts grow.
      const available = Math.min(area.clientHeight * .7, area.clientHeight - 2 * (header + 12));
      const capacity = opacity > 0 && area.clientHeight > header + footer + 24 && area.scrollHeight > area.clientHeight + 1 ? Math.max(0, Math.floor(available / pitch)) : 0;
      if (capacity < 2) {
        setHovered(null); setFocused(null);
        if (area.ownerDocument.activeElement?.closest(".dl-work-history") && area.parentElement?.contains(area.ownerDocument.activeElement)) composer?.querySelector<HTMLTextAreaElement>("textarea:not(:disabled)")?.focus({preventScroll: true});
      }
      const rects = anchors.map(node => node?.getBoundingClientRect() ?? {top: 0, bottom: 0});
      const next = {...historyViewport(rects, viewport.top + header * scale, viewport.bottom - footer * scale), capacity, opacity};
      setPosition(old => old.current === next.current && old.capacity === next.capacity && old.opacity === next.opacity && old.visible.length === next.visible.length && old.visible.every((index, slot) => index === next.visible[slot]) ? old : next);
    };
    const schedule = () => { if (!frame) frame = requestAnimationFrame(() => {frame = 0; measure();}); };
    const observer = new ResizeObserver(schedule);
    observer.observe(area); observer.observe(list);
    if (controls) observer.observe(controls);
    if (heading) observer.observe(heading);
    area.addEventListener("scroll", schedule, {passive: true});
    measure();
    return () => {observer.disconnect(); area.removeEventListener("scroll", schedule); cancelAnimationFrame(frame);};
  }, [messages, scroll]);

  if (messages.length < 2 || position.capacity < 2) return null;
  const stops = historyStops(messages.length, position.capacity);
  return <nav className="dl-work-history" style={{"--dl-history-opacity": position.opacity} as CSSProperties} aria-label={t("conversation_pane.history_navigation")} onPointerMove={event => {
    if (event.pointerType === "touch") return;
    const bounds = event.currentTarget.getBoundingClientRect();
    setHovered(Math.max(0, Math.min(stops.length - 1, Math.floor((event.clientY - bounds.top) / (bounds.height / stops.length)))));
  }} onPointerLeave={() => setHovered(null)}>
    {stops.map((stop, index) => {
      const message = messages[stop.target];
      const id = messageAnchor(message);
      const title = messageDisplayText(message) || message.attachments[0]?.name || t("conversation_pane.history_message");
      const visible = position.visible.some(item => item >= stop.first && item <= stop.last);
      const width = preview == null ? 6 : tickWidths[Math.abs(index - preview)] ?? 6;
      return <Tooltip key={message.id} open={preview === index}>
        <TooltipTrigger asChild><button type="button" aria-label={t("conversation_pane.jump_to_message", {message: title.slice(0, 80)})} aria-controls={id} aria-current={position.current >= stop.first && position.current <= stop.last ? "location" : undefined} data-visible={visible || undefined} data-preview={preview === index || undefined} style={{"--dl-history-tick": `${width}px`} as CSSProperties} onFocus={event => {if (event.currentTarget.matches(":focus-visible")) setFocused(index);}} onBlur={() => setFocused(null)} onClick={() => {
          onNavigate(); scroll.current?.querySelector<HTMLElement>(`#${CSS.escape(id)}`)?.scrollIntoView({block: "start", behavior: motionScrollBehavior()});
        }}><span aria-hidden="true" /></button></TooltipTrigger>
        <TooltipContent side="right" sideOffset={8} showArrow={false} collisionPadding={12} className="dl-work-history-preview" onEscapeKeyDown={event => {event.preventDefault(); event.stopPropagation(); setHovered(null); setFocused(null);}}>
          <strong>{title}</strong>
          {message.reply && <div className="dl-work-history-reply"><ReactMarkdown skipHtml allowedElements={["p", "ul", "ol", "li", "strong", "em", "code", "del", "br"]} unwrapDisallowed>{message.reply.slice(0, 800)}</ReactMarkdown></div>}
        </TooltipContent>
      </Tooltip>;
    })}
  </nav>;
}
