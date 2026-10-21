import { t } from "@/lib/i18n";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Field, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { accountInfo, createAccount, loginAccount, saveAccountConnection, type AccountInfo } from "@/lib/account-api";
import { accountErrorCode } from "@/lib/account-errors";
import { AccountError } from "@/components/account-error";
import { AutoSave } from "@/lib/auto-save";
import { validConnection } from "@/lib/device-settings";

export function AccessGate({ onAuthenticated }: { onAuthenticated(): void | Promise<void> }) {
  const [info, setInfo] = useState<AccountInfo | null>(null);
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [creating, setCreating] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [connection, setConnection] = useState("");
  const [connectionError, setConnectionError] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const mounted = useRef(true);
  const saveFailed = useRef(false);
  const saver = useRef<AutoSave<string | null> | null>(null);
  if (!saver.current) saver.current = new AutoSave(saveAccountConnection, cause => {
    saveFailed.current = true;
    if (mounted.current) setError(accountErrorCode(cause));
  }, () => { saveFailed.current = false; });

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; void saver.current?.flush(); };
  }, []);
  useEffect(() => {
    let live = true;
    setError(null);
    void accountInfo().then(value => {
      if (!live) return;
      setInfo(value); setConnection(value.relay_origin ?? "");
      if (value.name) setName(value.name);
    }).catch(cause => { if (live) setError(accountErrorCode(cause)); });
    return () => { live = false; };
  }, [attempt]);

  async function connect(event: FormEvent) {
    event.preventDefault();
    if (pending || !password || !name.trim() || !info) return;
    if (!creating && !validConnection(connection)) { setConnectionError(true); return; }
    if (creating && password !== confirmation) { setError("login_password_mismatch"); return; }
    setPending(true); setError(null);
    try {
      await saver.current?.flush();
      if (saveFailed.current) return;
      if (creating) await createAccount(name.trim(), password);
      else await loginAccount(name.trim(), password);
      await onAuthenticated(); setPassword(""); setConfirmation("");
    } catch (cause) { setError(accountErrorCode(cause)); }
    finally { setPending(false); }
  }

  function changeConnection(value: string) {
    setConnection(value); setConnectionError(false);
    if (validConnection(value)) saver.current?.queue(value || null);
  }

  return <section className="dl-access-gate">
    <header className="dl-access-heading">
      <div className="dl-access-brand"><img src="/logo-mark.svg" alt="" className="dl-brand-mark" /><span>Daat Locus</span></div>
      <h1>{creating ? t("access_gate.create_account") : t("access_gate.sign_in")}</h1>
    </header>
    {!info ? <div className="dl-access-status" role="status">{error ? <><p><AccountError code={error} /></p><Button variant="outline" onClick={() => setAttempt(value => value + 1)}>{t("app.retry")}</Button></> : <><Spinner />{t("app.connecting")}</>}</div> : <form onSubmit={connect} className="dl-access-form"><FieldGroup>
      <Field data-disabled={pending}><FieldLabel htmlFor="dl-account-name">{t("access_gate.account_name")}</FieldLabel><Input id="dl-account-name" value={name} autoComplete="username" autoFocus={!name} disabled={pending} required maxLength={128} onChange={event => { setName(event.target.value); setError(null); }} /></Field>
      <Field data-disabled={pending}><FieldLabel htmlFor="dl-access-credential">{creating ? t("access_gate.new_password") : t("access_gate.password")}</FieldLabel><Input id="dl-access-credential" type="password" value={password} autoFocus={Boolean(name)} disabled={pending} required minLength={creating ? 12 : undefined} autoComplete={creating ? "new-password" : "current-password"} spellCheck={false} maxLength={1024} onChange={event => { setPassword(event.target.value); setError(null); }} /></Field>
      {creating && <Field data-invalid={Boolean(error)} data-disabled={pending}><FieldLabel htmlFor="dl-account-confirmation">{t("access_gate.confirm_password")}</FieldLabel><Input id="dl-account-confirmation" type="password" autoComplete="new-password" value={confirmation} disabled={pending} required maxLength={1024} aria-invalid={Boolean(error)} onChange={event => { setConfirmation(event.target.value); setError(null); }} /></Field>}
      {!creating && !info.registered && <Field data-invalid={connectionError}><FieldLabel htmlFor="dl-account-connection">{t("access_gate.public_connection_service")}</FieldLabel><Input id="dl-account-connection" type="url" placeholder="wss://" value={connection} disabled={pending} aria-invalid={connectionError} onBlur={() => setConnectionError(!validConnection(connection))} onChange={event => changeConnection(event.target.value)} /><FieldError>{connectionError ? t("account_editor.invalid_connection") : null}</FieldError></Field>}
      {error && <FieldError><AccountError code={error} /></FieldError>}
      <Button type="submit" disabled={pending || !password || !name.trim()} size="lg">{pending && <Spinner data-icon="inline-start" />}{pending ? creating ? t("access_gate.creating_account") : t("access_gate.signing_in") : creating ? t("access_gate.create_account") : t("access_gate.sign_in")}</Button>
    </FieldGroup></form>}
    {info?.can_create && <Button variant="ghost" disabled={pending} onClick={() => { setCreating(value => !value); setError(null); }}>{creating ? t("access_gate.back_to_sign_in") : t("access_gate.create_account")}</Button>}
  </section>;
}
