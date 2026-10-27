import { useEffect, useRef } from "react";
import { SearchIcon, XIcon } from "lucide-react";
import { t } from "@/lib/i18n";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from "@/components/ui/input-group";
import { Separator } from "@/components/ui/separator";
import { Spinner } from "@/components/ui/spinner";

export function FloatingSearchBar({ open, query, onQueryChange, onClose, busy, controls, label = t("app.search_conversation") }: { open: boolean; query: string; onQueryChange(query: string): void; onClose(): void; busy: boolean; controls: string; label?: string }) {
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement;
    input.current?.focus({preventScroll: true});
    return () => { if (previous instanceof HTMLElement && previous.isConnected) previous.focus({preventScroll: true}); };
  }, [open]);
  return <div className="dl-floating-search" data-open={open} inert={!open || undefined} aria-hidden={!open || undefined} role="search" aria-label={label} onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); onClose(); } }}>
    <InputGroup className="dl-floating-search-input">
      <InputGroupAddon>{busy ? <Spinner /> : <SearchIcon aria-hidden="true" />}</InputGroupAddon>
      <InputGroupInput ref={input} role="searchbox" aria-label={label} aria-controls={controls} autoComplete="off" value={query} onChange={event => onQueryChange(event.target.value)} placeholder={t("search_dialog.search")} />
      <InputGroupAddon align="inline-end"><Separator orientation="vertical" /><InputGroupButton size="icon-sm" aria-label={t("search_dialog.close_search")} onClick={onClose}><XIcon /></InputGroupButton></InputGroupAddon>
    </InputGroup>
  </div>;
}
