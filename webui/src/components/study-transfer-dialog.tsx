import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { DownloadIcon, FolderOpenIcon, UploadIcon } from "lucide-react";

import { DirBrowserDialog } from "@/components/dir-browser-dialog";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import {
  commitObsidianImport,
  downloadStudyExport,
  exportStudyGraph,
  previewObsidianImport,
  type ObsidianImportPreview,
  type ObsidianImportResult,
  type StudyExportArtifact,
} from "@/lib/daemon-api";

type StudyTransferDialogProps = {
  mode: "import" | "export";
  sessionId: string | null;
  modules: Array<{ id: string; title: string }>;
  knownNodes: Array<{ id: string; title: string }>;
  onClose: () => void;
  onImported?: () => void;
};

export function StudyTransferDialog({
  mode,
  sessionId,
  modules,
  knownNodes,
  onClose,
  onImported,
}: StudyTransferDialogProps) {
  const { t } = useTranslation();
  const nodeTitles = useMemo(
    () => new Map(knownNodes.map((node) => [node.id, node.title])),
    [knownNodes],
  );

  const [step, setStep] = useState<"source" | "preview" | "result">("source");
  const [vaultDir, setVaultDir] = useState("");
  const [targetMode, setTargetMode] = useState<"new" | "existing">("new");
  const [existingModuleId, setExistingModuleId] = useState<string>(
    modules[0]?.id ?? "",
  );
  const [mergeDuplicates, setMergeDuplicates] = useState(true);
  const [preview, setPreview] = useState<ObsidianImportPreview | null>(null);
  const [importResult, setImportResult] = useState<ObsidianImportResult | null>(
    null,
  );
  const [dirBrowserOpen, setDirBrowserOpen] = useState(false);

  const [format, setFormat] = useState<"obsidian_vault" | "json">(
    "obsidian_vault",
  );
  const [scope, setScope] = useState<string>("all");
  const [includeQuestions, setIncludeQuestions] = useState(false);
  const [exported, setExported] = useState<StudyExportArtifact | null>(null);

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const isMock = sessionId?.startsWith("mock-") ?? false;

  const busyLabel =
    mode === "import"
      ? step === "preview"
        ? t("study.transfer.importing")
        : t("study.transfer.previewing")
      : t("study.transfer.exporting");

  async function handlePreview() {
    if (!sessionId || busy) {
      return;
    }
    if (!vaultDir.trim()) {
      setError(t("study.transfer.vaultDirRequired"));
      return;
    }
    setBusy(true);
    setError(null);
    if (isMock) {
      setPreview(MOCK_IMPORT_PREVIEW);
      setStep("preview");
      setBusy(false);
      return;
    }
    try {
      const nextPreview = await previewObsidianImport({
        sessionId,
        vaultDir: vaultDir.trim(),
        moduleId: targetMode === "existing" ? existingModuleId : null,
      });
      setPreview(nextPreview);
      setStep("preview");
    } catch (previewError) {
      setError(
        previewError instanceof Error
          ? previewError.message
          : String(previewError),
      );
    } finally {
      setBusy(false);
    }
  }

  async function handleCommit() {
    if (!sessionId || busy || !preview) {
      return;
    }
    setBusy(true);
    setError(null);
    if (isMock) {
      setImportResult({
        module_id: "mock-module",
        module_title: preview.proposed_module_title,
        created_nodes: preview.new_node_count,
        merged_nodes: mergeDuplicates ? preview.duplicate_count : 0,
        created_edges: preview.new_node_count,
        skipped_duplicates: mergeDuplicates ? 0 : preview.duplicate_count,
        skipped_links: preview.dangling_link_count,
      });
      setStep("result");
      setBusy(false);
      onImported?.();
      return;
    }
    try {
      const result = await commitObsidianImport({
        sessionId,
        vaultDir: vaultDir.trim(),
        moduleId: targetMode === "existing" ? existingModuleId : null,
        mergeDuplicates,
      });
      setImportResult(result);
      setStep("result");
      onImported?.();
    } catch (commitError) {
      setError(
        commitError instanceof Error ? commitError.message : String(commitError),
      );
    } finally {
      setBusy(false);
    }
  }

  async function handleExport() {
    if (!sessionId || busy) {
      return;
    }
    setBusy(true);
    setError(null);
    if (isMock) {
      setExported({
        file_name:
          format === "json" ? "study-graph-mock.zip" : "study-vault-mock.zip",
        path: "mock/study-export.zip",
        size_bytes: 184_320,
      });
      setBusy(false);
      return;
    }
    try {
      const artifact = await exportStudyGraph({
        sessionId,
        format,
        moduleId: scope === "all" ? null : scope,
        includeQuestions: format === "json" && includeQuestions,
      });
      await downloadStudyExport({
        path: artifact.path,
        fileName: artifact.file_name,
      });
      setExported(artifact);
    } catch (exportError) {
      setError(
        exportError instanceof Error ? exportError.message : String(exportError),
      );
    } finally {
      setBusy(false);
    }
  }

  function handleOpenChange(open: boolean) {
    if (!open && !busy) {
      onClose();
    }
  }

  return (
    <>
      <Dialog open onOpenChange={handleOpenChange}>
        <DialogContent className="sm:max-w-xl">
          <DialogHeader>
            <DialogTitle>
              {mode === "import"
                ? t("study.transfer.importTitle")
                : t("study.transfer.exportTitle")}
            </DialogTitle>
            <DialogDescription>
              {mode === "import"
                ? t("study.transfer.importDescription")
                : t("study.transfer.exportDescription")}
            </DialogDescription>
          </DialogHeader>

          {mode === "import" ? (
            <div className="flex max-h-[60vh] flex-col gap-4 overflow-y-auto pr-1">
              {step === "source" ? (
                <>
                  <label className="flex flex-col gap-1.5">
                    <span className="text-xs font-medium text-muted-foreground">
                      {t("study.transfer.vaultDirLabel")}
                    </span>
                    <div className="flex items-center gap-2">
                      <Input
                        value={vaultDir}
                        onChange={(event) => setVaultDir(event.target.value)}
                        placeholder={t("study.transfer.vaultDirPlaceholder")}
                        className="min-w-0 flex-1"
                      />
                      <Button
                        type="button"
                        variant="outline"
                        onClick={() => setDirBrowserOpen(true)}
                      >
                        <FolderOpenIcon data-icon="inline-start" />
                        {t("study.transfer.browse")}
                      </Button>
                    </div>
                  </label>

                  <div className="flex flex-col gap-1.5">
                    <span className="text-xs font-medium text-muted-foreground">
                      {t("study.transfer.targetLabel")}
                    </span>
                    <div className="flex flex-wrap items-center gap-2">
                      <Button
                        type="button"
                        size="sm"
                        variant={targetMode === "new" ? "secondary" : "outline"}
                        onClick={() => setTargetMode("new")}
                      >
                        {t("study.transfer.newModule")}
                      </Button>
                      <Button
                        type="button"
                        size="sm"
                        variant={
                          targetMode === "existing" ? "secondary" : "outline"
                        }
                        disabled={modules.length === 0}
                        onClick={() => setTargetMode("existing")}
                      >
                        {t("study.transfer.existingModule")}
                      </Button>
                      {targetMode === "existing" ? (
                        <Select
                          value={existingModuleId}
                          onValueChange={setExistingModuleId}
                        >
                          <SelectTrigger className="h-8 w-56">
                            <SelectValue
                              placeholder={t("study.transfer.existingModule")}
                            />
                          </SelectTrigger>
                          <SelectContent>
                            {modules.map((module) => (
                              <SelectItem key={module.id} value={module.id}>
                                {module.title}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                      ) : null}
                    </div>
                  </div>
                </>
              ) : null}

              {step === "preview" && preview ? (
                <>
                  <div className="grid grid-cols-2 gap-2 text-sm sm:grid-cols-3">
                    <PreviewStat
                      label={t("study.transfer.notesFound")}
                      value={preview.notes_found}
                    />
                    <PreviewStat
                      label={t("study.transfer.newNodes")}
                      value={preview.new_node_count}
                    />
                    <PreviewStat
                      label={t("study.transfer.duplicates")}
                      value={preview.duplicate_count}
                    />
                    <PreviewStat
                      label={t("study.transfer.dangling")}
                      value={preview.dangling_link_count}
                    />
                    <PreviewStat
                      label={t("study.transfer.skipped")}
                      value={preview.skipped_file_count}
                    />
                  </div>

                  {preview.truncated ? (
                    <Alert>
                      <AlertDescription className="text-xs">
                        {t("study.transfer.truncated")}
                      </AlertDescription>
                    </Alert>
                  ) : null}

                  {preview.duplicates.length > 0 ? (
                    <PreviewList
                      title={t("study.transfer.duplicatesTitle")}
                      items={preview.duplicates.map(
                        (duplicate) =>
                          `${duplicate.title} → ${
                            nodeTitles.get(duplicate.existing_node_id) ??
                            duplicate.existing_node_id
                          }`,
                      )}
                    />
                  ) : null}
                  {preview.dangling_links.length > 0 ? (
                    <PreviewList
                      title={t("study.transfer.danglingTitle")}
                      items={preview.dangling_links.map(
                        (link) => `${link.from_title} → [[${link.target}]]`,
                      )}
                    />
                  ) : null}
                  {preview.skipped_files.length > 0 ? (
                    <PreviewList
                      title={t("study.transfer.skippedTitle")}
                      items={preview.skipped_files.map(
                        (file) => `${file.rel_path} (${file.reason})`,
                      )}
                    />
                  ) : null}

                  <label className="flex items-center justify-between gap-3 rounded-md border px-3 py-2">
                    <span className="text-sm">
                      {t("study.transfer.mergeDuplicates")}
                    </span>
                    <Switch
                      checked={mergeDuplicates}
                      onCheckedChange={setMergeDuplicates}
                    />
                  </label>
                </>
              ) : null}

              {step === "result" && importResult ? (
                <Alert>
                  <AlertDescription className="text-sm">
                    {t("study.transfer.importDone", {
                      created: importResult.created_nodes,
                      merged: importResult.merged_nodes,
                      edges: importResult.created_edges,
                      module: importResult.module_title,
                    })}
                  </AlertDescription>
                </Alert>
              ) : null}

              {error ? (
                <Alert variant="destructive">
                  <AlertDescription className="text-xs">{error}</AlertDescription>
                </Alert>
              ) : null}
            </div>
          ) : (
            <div className="flex flex-col gap-4">
              <label className="flex flex-col gap-1.5">
                <span className="text-xs font-medium text-muted-foreground">
                  {t("study.transfer.scopeLabel")}
                </span>
                <Select value={scope} onValueChange={setScope}>
                  <SelectTrigger className="h-8">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">
                      {t("study.transfer.scopeAll")}
                    </SelectItem>
                    {modules.map((module) => (
                      <SelectItem key={module.id} value={module.id}>
                        {module.title}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </label>

              <div className="flex flex-col gap-1.5">
                <span className="text-xs font-medium text-muted-foreground">
                  {t("study.transfer.formatLabel")}
                </span>
                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    type="button"
                    size="sm"
                    variant={format === "obsidian_vault" ? "secondary" : "outline"}
                    onClick={() => setFormat("obsidian_vault")}
                  >
                    {t("study.transfer.formatVault")}
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    variant={format === "json" ? "secondary" : "outline"}
                    onClick={() => setFormat("json")}
                  >
                    {t("study.transfer.formatJson")}
                  </Button>
                </div>
              </div>

              {format === "json" ? (
                <label className="flex items-center justify-between gap-3 rounded-md border px-3 py-2">
                  <span className="text-sm">
                    {t("study.transfer.includeQuestions")}
                  </span>
                  <Switch
                    checked={includeQuestions}
                    onCheckedChange={setIncludeQuestions}
                  />
                </label>
              ) : null}

              {exported ? (
                <Alert>
                  <AlertDescription className="text-sm">
                    {t("study.transfer.exportReady", {
                      name: exported.file_name,
                      size: Math.max(
                        1,
                        Math.round(exported.size_bytes / 1024),
                      ),
                    })}
                  </AlertDescription>
                </Alert>
              ) : null}

              {error ? (
                <Alert variant="destructive">
                  <AlertDescription className="text-xs">{error}</AlertDescription>
                </Alert>
              ) : null}
            </div>
          )}

          <DialogFooter>
            {busy ? (
              <span className="mr-auto flex items-center gap-2 text-xs text-muted-foreground">
                <Spinner className="size-3.5" />
                {busyLabel}
              </span>
            ) : null}
            {mode === "import" ? (
              step === "source" ? (
                <>
                  <Button
                    type="button"
                    variant="outline"
                    disabled={busy}
                    onClick={onClose}
                  >
                    {t("common.cancel")}
                  </Button>
                  <Button
                    type="button"
                    disabled={busy || !sessionId}
                    onClick={() => void handlePreview()}
                  >
                    <UploadIcon data-icon="inline-start" />
                    {t("study.transfer.preview")}
                  </Button>
                </>
              ) : step === "preview" ? (
                <>
                  <Button
                    type="button"
                    variant="outline"
                    disabled={busy}
                    onClick={() => setStep("source")}
                  >
                    {t("study.transfer.back")}
                  </Button>
                  <Button
                    type="button"
                    disabled={
                      busy ||
                      ((preview?.new_node_count ?? 0) === 0 &&
                        !(mergeDuplicates && (preview?.duplicate_count ?? 0) > 0))
                    }
                    onClick={() => void handleCommit()}
                  >
                    {t("study.transfer.confirmImport", {
                      count: preview?.new_node_count ?? 0,
                    })}
                  </Button>
                </>
              ) : (
                <Button type="button" onClick={onClose}>
                  {t("study.transfer.close")}
                </Button>
              )
            ) : (
              <>
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy}
                  onClick={onClose}
                >
                  {t("common.cancel")}
                </Button>
                <Button
                  type="button"
                  disabled={busy || !sessionId}
                  onClick={() => void handleExport()}
                >
                  <DownloadIcon data-icon="inline-start" />
                  {t("study.transfer.exportButton")}
                </Button>
              </>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <DirBrowserDialog
        open={dirBrowserOpen}
        onOpenChange={setDirBrowserOpen}
        onSelect={(path) => {
          setVaultDir(path);
          setDirBrowserOpen(false);
        }}
      />
    </>
  );
}

const MOCK_IMPORT_PREVIEW: ObsidianImportPreview = {
  vault_dir: "C:\\Users\\you\\MyVault",
  proposed_module_title: "MyVault",
  notes_found: 214,
  new_node_count: 198,
  duplicate_count: 9,
  dangling_link_count: 12,
  skipped_file_count: 5,
  truncated: false,
  duplicates: [
    {
      title: "Group",
      rel_path: "Algebra/Group.md",
      existing_node_id: "polynomials",
    },
    {
      title: "Limits",
      rel_path: "Calculus/Limits.md",
      existing_node_id: "limits",
    },
  ],
  dangling_links: [
    { from_title: "Numbers", target: "Field axioms" },
    { from_title: "Series", target: "Ratio test" },
  ],
  skipped_files: [
    { rel_path: "notes/huge.md", reason: "file exceeds 1024 KiB" },
  ],
};

function PreviewStat({ label, value }: { label: string; value: number }) {
  return (
    <div className="flex flex-col rounded-md border px-2.5 py-1.5">
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="text-base font-semibold tabular-nums">{value}</span>
    </div>
  );
}

function PreviewList({ title, items }: { title: string; items: string[] }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-xs font-medium text-muted-foreground">{title}</span>
      <div className="flex max-h-32 flex-col overflow-y-auto rounded-md border">
        {items.map((item, index) => (
          <span
            key={`${item}-${index}`}
            className="truncate border-b px-2 py-1 text-xs last:border-b-0"
            title={item}
          >
            {item}
          </span>
        ))}
      </div>
    </div>
  );
}
