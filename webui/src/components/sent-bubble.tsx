import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

export type SendOrigin = { messageId: string; left: number; top: number; width: number; height: number };

/** Keep the destination in flow while a visual copy contracts from the composer. */
export function SentBubble({ origin, children }: { origin?: SendOrigin; children: ReactNode }) {
  const target = useRef<HTMLDivElement>(null);
  const flight = useRef<HTMLDivElement>(null);
  const [destination, setDestination] = useState<DOMRect | null>(null);
  const [arrived, setArrived] = useState(false);
  useLayoutEffect(() => {
    if (!origin || !target.current) return;
    setDestination(target.current.getBoundingClientRect());
  }, [origin]);
  useLayoutEffect(() => {
    if (!origin || !destination || !flight.current || arrived) return;
    const frame = (left: number, top: number, width: number, height: number) => ({left:`${left}px`,top:`${top}px`,width:`${width}px`,height:`${height}px`});
    const frames = (end: DOMRect) => [
      frame(origin.left,origin.top,origin.width,origin.height),
      {...frame(origin.left+(end.left-origin.left)*.78,origin.top+(end.top-origin.top)*.6,origin.width+(end.width-origin.width)*.8,origin.height+(end.height-origin.height)*.8),offset:.55},
      frame(end.left,end.top,end.width,end.height),
    ];
    const animation = flight.current.animate(frames(destination),{duration:440,easing:"cubic-bezier(.22,1,.36,1)"});
    // Receipts and neighboring bubbles can still be settling during the flight.
    let tick = 0;
    const track = () => {
      if (target.current) (animation.effect as KeyframeEffect).setKeyframes(frames(target.current.getBoundingClientRect()));
      tick = requestAnimationFrame(track);
    };
    tick = requestAnimationFrame(track);
    void animation.finished.then(() => setArrived(true), () => {});
    return () => { cancelAnimationFrame(tick); animation.cancel(); };
  }, [origin,destination,arrived]);
  const flying = Boolean(origin && !arrived);
  return <>
    <div ref={target} className="dl-bubble dl-bubble-user" style={flying ? {visibility:"hidden"} : undefined}>{children}</div>
    {flying && destination && createPortal(<div ref={flight} className="dl-primary-turn dl-message-flight" aria-hidden="true"
      style={{left:destination.left,top:destination.top,width:destination.width,height:destination.height}}><div className="dl-bubble dl-bubble-user">{children}</div></div>,document.body)}
  </>;
}
