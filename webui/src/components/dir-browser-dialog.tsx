import {
  ArrowLeftIcon,
  FolderIcon,
  FolderOpenIcon,
  HardDriveIcon,
  LoaderCircleIcon,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { listDirs, type DirListResponse } from "@/lib/daemon-api";

function pathSegments(p: string): { label: string; full: string }[] {
  if (!p) return [];
  const parts = p.replace(/\\/g, "/").split("/").filter(Boolean);
  const segs: { label: string; full: string }[] = [];
  for (let i = 0; i < parts.length; i++) {
    const full =
      p.match(/^[A-Z]:/i) && i === 0
        ? parts[0] + "\\"
        : segs[i - 1]?.full.replace(/\\+$/, "") + "\\" + parts[i];
    segs.push({ label: parts[i], full });
  }
  return segs;
}

export function DirBrowserDialog({
  open,
  onOpenChange,
  onSelect,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (dirPath: string) => void;
}) {
  const { t } = useTranslation();
  const [currentPath, setCurrentPath] = useState("");
  const [data, setData] = useState<DirListResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [inputValue, setInputValue] = useState("");
  const abortRef = useRef<AbortController | null>(null);

  const firstLoadRef = useRef(false);

  const fetchDir = useCallback(async (path?: string) => {
    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;
    setLoading(true);
    setError(null);
    try {
      const result = await listDirs({ path, signal: controller.signal });
      if (!controller.signal.aborted) {
        setData(result);
        setCurrentPath(result.path);
        setInputValue(result.path);
      }
    } catch (err) {
      if (!controller.signal.aborted) {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (!controller.signal.aborted) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    if (open && !firstLoadRef.current) {
      firstLoadRef.current = true;
      fetchDir();
    }
    if (!open) {
      firstLoadRef.current = false;
    }
  }, [open, fetchDir]);

  const segs = pathSegments(currentPath);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("dirBrowser.title")}</DialogTitle>
          <DialogDescription>
            {t("dirBrowser.description")}
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-3">
          {/* Breadcrumb */}
          <Breadcrumb>
            <BreadcrumbList>
              <BreadcrumbItem>
                <Button
                  variant="link"
                  size="sm"
                  className="h-auto p-0"
                  onClick={() => fetchDir()}
                >
                  <HardDriveIcon />
                  <span className="sr-only">{t("dirBrowser.root")}</span>
                </Button>
              </BreadcrumbItem>
              {segs.length > 0 && <BreadcrumbSeparator />}
              {segs.map((seg, i) => (
                <BreadcrumbItem key={seg.full}>
                  {i < segs.length - 1 ? (
                    <>
                      <Button
                        variant="link"
                        size="sm"
                        className="h-auto p-0"
                        onClick={() => fetchDir(seg.full)}
                      >
                        {seg.label}
                      </Button>
                      <BreadcrumbSeparator />
                    </>
                  ) : (
                    <span className="text-sm font-medium">{seg.label}</span>
                  )}
                </BreadcrumbItem>
              ))}
            </BreadcrumbList>
          </Breadcrumb>

          {/* Input */}
          <div className="flex gap-2">
            <Input
              placeholder={t("dirBrowser.pathPlaceholder")}
              value={inputValue}
              onChange={(e) => setInputValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") fetchDir(inputValue || undefined);
              }}
            />
            <Button
              type="button"
              variant="secondary"
              size="sm"
              onClick={() => fetchDir(inputValue || undefined)}
              disabled={loading}
            >
              {t("dirBrowser.go")}
            </Button>
          </div>
        </div>

        <Separator />

        {/* Directory list */}
        <ScrollArea className="h-52 rounded-md border">
          {loading && (
            <div className="flex items-center justify-center h-full gap-2 text-muted-foreground">
              <LoaderCircleIcon className="animate-spin" />
              <span className="text-sm">{t("dirBrowser.loading")}</span>
            </div>
          )}
          {error && !loading && (
            <div className="flex items-center justify-center h-full">
              <span className="text-sm text-destructive">{error}</span>
            </div>
          )}
          {!loading && !error && data && (
            <div className="flex flex-col">
              {data.parent != null && (
                <Button
                  variant="ghost"
                  className="justify-start rounded-none"
                  onClick={() => fetchDir(data.parent ?? undefined)}
                >
                  <ArrowLeftIcon />
                  <span>..</span>
                </Button>
              )}
              {data.entries.length === 0 && (
                <div className="flex items-center justify-center h-16">
                  <span className="text-sm text-muted-foreground">
                    {t("dirBrowser.noDirs")}
                  </span>
                </div>
              )}
              {data.entries.map((entry) => (
                <Button
                  key={entry.name}
                  variant="ghost"
                  className="justify-start rounded-none"
                  onClick={() => fetchDir(currentPath ? `${currentPath}\\${entry.name}` : entry.name)}
                >
                  <FolderIcon data-icon="inline-start" />
                  <span className="truncate">{entry.name}</span>
                </Button>
              ))}
            </div>
          )}
        </ScrollArea>

        <DialogFooter>
          <DialogClose asChild>
            <Button type="button" variant="outline">
              {t("dirBrowser.cancel")}
            </Button>
          </DialogClose>
          <Button
            type="button"
            onClick={() => {
              const selected = inputValue || currentPath;
              if (selected) onSelect(selected);
            }}
            disabled={!inputValue && !currentPath}
          >
            <FolderOpenIcon data-icon="inline-start" />
            {t("dirBrowser.select")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
