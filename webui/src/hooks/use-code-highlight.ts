import { useEffect, useState } from "react";
import { isCodeTheme } from "@/lib/appearance";
import { highlightCodeWithShiki, type ShikiHighlightedCode } from "@/lib/shiki-highlight";

export function useCodeHighlight(code: string, language: string) {
  const [highlight, setHighlight] = useState<{ code: string; language: string; value: ShikiHighlightedCode | null } | null>(null);
  useEffect(() => {
    let live = true;
    let revision = 0;
    const update = () => {
      const current = ++revision;
      const root = document.documentElement;
      void highlightCodeWithShiki(code, language, root.classList.contains("dark") ? "dark" : "light", isCodeTheme(root.dataset.codeTheme) ? root.dataset.codeTheme : "github").then(value => {
        if (live && current === revision) setHighlight({code,language,value});
      }).catch(() => { if (live && current === revision) setHighlight(null); });
    };
    update();
    const observer = new MutationObserver(update);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-code-theme"] });
    return () => { live = false; observer.disconnect(); };
  }, [code, language]);
  return highlight?.code === code && highlight.language === language ? highlight.value : null;
}
