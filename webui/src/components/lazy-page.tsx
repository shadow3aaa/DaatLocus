import { useEffect, useRef, useState } from "react";
import { t } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";

// Mount only inside an open disclosure. Each visible boundary consumes one
// cursor; the next page is observed again after React has laid out its content.
export function LazyPage({ cursor, onLoad, errorMessage }: { cursor: number; onLoad(signal: AbortSignal): Promise<unknown>; errorMessage: string }) {
  const boundary = useRef<HTMLDivElement>(null);
  const load = useRef(onLoad);
  load.current = onLoad;
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    const node = boundary.current;
    if (!node) return;
    const controller = new AbortController();
    let requested = false;
    setLoading(false);
    setFailed(false);
    const observer = new IntersectionObserver(entries => {
      if (requested || !entries.some(entry => entry.isIntersecting) || controller.signal.aborted) return;
      requested = true;
      observer.disconnect();
      setLoading(true);
      void load.current(controller.signal).catch(() => {
        if (!controller.signal.aborted) setFailed(true);
      }).finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    }, { root: node.closest(".dl-conversation-scroll"), rootMargin: "160px 0px" });
    observer.observe(node);
    return () => { observer.disconnect(); controller.abort(); };
  }, [cursor, retry]);
  return <div ref={boundary} className="min-h-px" aria-busy={loading}>
    {loading && <div className="flex justify-center py-2"><Spinner /></div>}
    {failed && <div className="dl-client-error" role="alert"><span>{errorMessage}</span><Button variant="ghost" size="sm" onClick={() => setRetry(value => value + 1)}>{t("app.retry")}</Button></div>}
  </div>;
}
