import { useReducer, useState } from "react";
import { t, apiErrorText } from "@/lib/i18n";
import { sendSessionMessage, uploadSessionAttachment, type Access, type Attachment } from "@/lib/actor-api";
import { createSession } from "@/lib/session-api";
import type { SkillReference } from "@/lib/skills";
import { Button } from "@/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Composer, type ComposerModel } from "@/components/composer";

type DraftFile = { id: string; file: File; url: string; uploaded?: { session: string; id: string } };
type Submission = { key: string; session?: string; message?: { key: string; content: string } };
/** The unsent first message survives leaving this page, like conversation drafts. */
const draft: { text: string; skills: SkillReference[]; files: DraftFile[]; model: string; submission?: Submission } = { text: "", skills: [], files: [], model: "" };
export function clearNewSessionDraft() {
  for (const item of draft.files) URL.revokeObjectURL(item.url);
  Object.assign(draft, { text: "", skills: [], files: [], model: "", submission: undefined });
}

const tooLarge = (file: File) => !file.size || file.size > (file.type.startsWith("image/") ? 10 : 32) * 1024 * 1024;
const errorText = (error: unknown) => error instanceof Error && error.message ? error.message : t("conversation_pane.attachment_upload_failed_try_again");

/** The empty session page: sending the first message creates the session. */
export function NewSession({ access, models, ready, onCreated, onConfigureModels }: {
  access: Access; models: ComposerModel[]; ready: boolean; onCreated(session: string): void; onConfigureModels(): void;
}) {
  const [, render] = useReducer((value: number) => value + 1, 0);
  const [busy, setBusy] = useState(false);
  const [preparing, setPreparing] = useState(false);
  const [error, setError] = useState("");
  const edit = (change: Partial<typeof draft>) => { Object.assign(draft, change); render(); };
  const attachments: Attachment[] = draft.files.map(item => ({ id: item.id, name: item.file.name, media_type: item.file.type || "application/octet-stream", size_bytes: item.file.size, uri: item.url }));

  async function files(selected: File[]) {
    if (preparing) return;
    setPreparing(true); setError("");
    try {
      const folderFiles = selected.filter(file => file.webkitRelativePath);
      let chosen = selected;
      if (folderFiles.length) {
        const { zipFolder } = await import("@/lib/folder-attachment");
        chosen = [await zipFolder(folderFiles)];
      }
      if (chosen.some(tooLarge)) throw new Error(apiErrorText("payload_too_large"));
      edit({ files: [...draft.files, ...chosen.map(file => ({ id: crypto.randomUUID(), file, url: URL.createObjectURL(file) }))] });
    } catch (failure) { setError(errorText(failure)); }
    finally { setPreparing(false); }
  }
  function remove(id: string) {
    const item = draft.files.find(file => file.id === id);
    if (item) URL.revokeObjectURL(item.url);
    edit({ files: draft.files.filter(file => file.id !== id) });
  }
  async function submit() {
    if (busy) return;
    const content = draft.text.trim();
    if (!content && !draft.files.length) return;
    setBusy(true); setError("");
    try {
      const submission = draft.submission ??= { key: crypto.randomUUID() };
      // A retried creation reuses its key, so a lost response cannot create a second session.
      submission.session ??= (await createSession({ model: draft.model || null }, submission.key)).id;
      const session = submission.session;
      for (const item of draft.files) {
        if (item.uploaded?.session === session) continue;
        const attachment = await uploadSessionAttachment(session, item.file);
        item.uploaded = { session, id: attachment.id };
      }
      const identity = JSON.stringify([content, draft.skills, draft.files.map(item => item.uploaded!.id)]);
      if (submission.message?.content !== identity) submission.message = { key: crypto.randomUUID(), content: identity };
      await sendSessionMessage(session, { content, attachments: draft.files.map(item => item.uploaded!.id), skills: draft.skills.length ? draft.skills : undefined }, submission.message.key);
      clearNewSessionDraft();
      onCreated(session);
    } catch (failure) {
      setError(failure instanceof Error && failure.message ? failure.message : t("sessions.create_failed"));
    } finally { setBusy(false); }
  }

  return <section className="dl-conversation dl-primary-conversation dl-new-session" aria-label={t("navigation.new_session")}>
    <div className="dl-conversation-scroll"><div className="dl-message-list">
      <Empty className="dl-conversation-empty"><EmptyHeader>
        <img src="/logo-mark.svg" alt="" className="dl-empty-logo dl-brand-mark" />
        <EmptyTitle>{ready ? "Daat Locus" : t("conversation_pane.set_up_model")}</EmptyTitle>
        {ready && <EmptyDescription>{t("conversation_pane.start_conversation")}</EmptyDescription>}
      </EmptyHeader>{!ready && <Button onClick={onConfigureModels}>{t("providers_settings.add_provider")}</Button>}</Empty>
    </div></div>
    <div className="dl-conversation-footer">
      {error && <div className="dl-client-error" role="alert"><span>{error}</span></div>}
      <Composer value={draft.text} onValueChange={text => edit({ text })} skills={draft.skills} onSkillsChange={skills => edit({ skills })} onSubmit={() => { void submit(); }}
        attachments={attachments} onFiles={selected => { void files(selected); }} onRemoveAttachment={remove} uploading={preparing} busy={busy}
        disabled={!ready} canManage={access.kind === "owner"} models={models} modelName={draft.model} onModelChange={model => edit({ model })}
        placeholder={ready ? t("composer.message") : t("conversation_pane.add_a_model_to_send_messages")} />
    </div>
  </section>;
}
