import { ArrowDownToLineIcon, DownloadIcon, FileIcon, FolderOpenIcon, ImageIcon, LoaderCircleIcon, MoreHorizontalIcon, Trash2Icon, XIcon } from "lucide-react";
import { useTranslation } from "react-i18next";
import { t } from "@/lib/i18n";
import { isAndroid } from "@/lib/mobile";
import type { BrowserLibrary, BrowserLibraryAction } from "@/lib/browser-workspace";
import { LazyPage } from "@/components/lazy-page";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Empty, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";

export function BrowserDownloads({ library, popup = false, onAction, onMore }: {
  library: BrowserLibrary;
  popup?: boolean;
  onAction(action: BrowserLibraryAction, value: string): void;
  onMore(signal: AbortSignal): Promise<unknown>;
}) {
  const { i18n } = useTranslation();
  return <div className="dl-browser-download-list" role="list">
    {library.downloads.map(item => {
      const Icon = item.status === "downloading" ? LoaderCircleIcon : /\.(png|jpe?g|gif|webp|avif|svg|bmp|ico)$/i.test(item.name) ? ImageIcon : FileIcon;
      const size = `${(item.bytes / 1024).toLocaleString(i18n.language, { maximumFractionDigits: 1 })} KB`;
      const time = new Date(item.time).toLocaleTimeString(i18n.language, { hour: "2-digit", minute: "2-digit" });
      return <div key={item.id} className="dl-browser-download-row" role="listitem">
        <span className="dl-browser-row-icon"><Icon className={item.status === "downloading" ? "animate-spin" : undefined}/></span>
        <div className="dl-browser-download-description"><strong title={item.name}>{item.name}</strong><small>{t(`browser_use.download_${item.status}`)}{(item.status === "complete" || item.bytes > 0) && ` · ${size}`}{` · ${time}`}</small>{item.status === "downloading" && <div role="progressbar" aria-label={item.name} className="dl-browser-download-progress"/>}</div>
        {popup ? <>
          <Button variant="ghost" size="icon-sm" disabled={item.status !== "complete"} aria-label={t(isAndroid() ? "browser_use.save_download" : "browser_use.reveal_download")} title={t(isAndroid() ? "browser_use.save_download" : "browser_use.reveal_download")} onClick={() => onAction(isAndroid() ? "save_download" : "reveal_download", item.id)}>{isAndroid() ? <ArrowDownToLineIcon/> : <FolderOpenIcon/>}</Button>
          <DropdownMenu modal={false}><DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm" aria-label={t("browser_use.download_actions", { name: item.name })}><MoreHorizontalIcon/></Button></DropdownMenuTrigger><DropdownMenuContent align="end" sideOffset={4} className="dl-browser-download-menu"><DropdownMenuGroup>
            <DropdownMenuItem disabled={item.status !== "complete"} onSelect={() => onAction("save_download", item.id)}><ArrowDownToLineIcon/>{t("browser_use.save_download")}</DropdownMenuItem>
            <DropdownMenuItem disabled={item.status === "downloading"} onSelect={() => onAction("remove_download", item.id)}><Trash2Icon/>{t("browser_use.remove_download")}</DropdownMenuItem>
          </DropdownMenuGroup></DropdownMenuContent></DropdownMenu>
        </> : <>
          <Button variant="ghost" size="icon-sm" disabled={item.status !== "complete"} aria-label={t("browser_use.save_download")} onClick={() => onAction("save_download", item.id)}><ArrowDownToLineIcon/></Button>
          <Button variant="ghost" size="icon-sm" disabled={item.status === "downloading"} aria-label={t("browser_use.remove_download")} onClick={() => onAction("remove_download", item.id)}><XIcon/></Button>
        </>}
      </div>;
    })}
    {!library.downloads.length && <Empty><EmptyHeader><EmptyMedia variant="icon"><DownloadIcon/></EmptyMedia><EmptyTitle>{t("browser_use.no_downloads")}</EmptyTitle></EmptyHeader></Empty>}
    {library.next_offset != null && <LazyPage cursor={library.next_offset} onLoad={onMore} errorMessage={t("lazy_page.load_failed")} />}
  </div>;
}
