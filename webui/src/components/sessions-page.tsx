import { useEffect, useState } from "react";
import { MessagesSquareIcon, MoreHorizontalIcon, PanelLeftIcon, SquarePenIcon } from "lucide-react";
import { useTranslation } from "react-i18next";
import { getCurrentWebUiLanguage, t } from "@/lib/i18n";
import { useSessionList } from "@/hooks/use-session-list";
import { Button } from "@/components/ui/button";
import { Empty, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { LazyPage } from "@/components/lazy-page";
import { SessionActions, sessionTitle } from "@/components/session-actions";

function activityTime(ms: number) {
  const date = new Date(ms), now = new Date();
  const sameDay = date.toDateString() === now.toDateString();
  return date.toLocaleString(getCurrentWebUiLanguage(), sameDay ? {timeStyle: "short"} : {dateStyle: "medium"});
}

export function SessionsPage({active, onNavigation, onNewSession, onSession, onAccessRevoked}: {
  active: boolean; onNavigation(): void; onNewSession(): void; onSession(address: string): void; onAccessRevoked(): void;
}) {
  useTranslation();
  const [archived, setArchived] = useState(false);
  const list = useSessionList(archived, active, onAccessRevoked);
  const [, tick] = useState(0);
  useEffect(() => { const timer = setInterval(() => tick(value => value + 1), 60_000); return () => clearInterval(timer); }, []);
  return <section className="dl-sessions-page" aria-label={t("sessions.title")}>
    <header className="dl-apps-header" data-window-drag-region><div className="dl-apps-title"><Button variant="ghost" size="icon-sm" className="dl-apps-mobile-navigation" aria-label={t("navigation.open_sidebar")} onClick={onNavigation}><PanelLeftIcon /></Button><h1>{t("sessions.title")}</h1></div><Button variant="outline" size="sm" onClick={onNewSession}><SquarePenIcon data-icon="inline-start" />{t("navigation.new_session")}</Button></header>
    <ToggleGroup type="single" size="sm" value={archived ? "archived" : "active"} onValueChange={value => { if (value) setArchived(value === "archived"); }} className="dl-sessions-filter" aria-label={t("sessions.filter")}>
      <ToggleGroupItem value="active">{t("sessions.active")}</ToggleGroupItem>
      <ToggleGroupItem value="archived">{t("sessions.archived")}</ToggleGroupItem>
    </ToggleGroup>
    <div className="dl-sessions-scroll" aria-busy={list.loading}>
      {list.loading && !list.entries.length && Array.from({length: 6}, (_, index) => <Skeleton key={index} className="mb-3 h-14 w-full rounded-xl" />)}
      {!list.failed && <ul className="dl-session-list">{list.entries.map(({address, session, source}) => <li key={address}>
        <button type="button" className="dl-session-row" onClick={() => onSession(address)}>
          <strong>{sessionTitle(session)}</strong>
          <small className="dl-session-meta">
            <span>{activityTime(session.last_activity_at_ms)}</span>
            {source.stage && <span>{source.label || t("sessions.other_device")}</span>}
            {session.status === "running" && <span className="dl-session-running"><span className="dl-session-dot" aria-hidden="true" />{session.working_actors > 0 ? t("sessions.working_count", {count: session.working_actors}) : t("actor_status.running")}</span>}
          </small>
        </button>
        <SessionActions address={address} session={session} onChanged={summary => list.update(source.stage, summary)} onDeleted={() => list.remove(source.stage, session.id)}
          trigger={<Button variant="ghost" size="icon-sm" className="dl-session-actions" aria-label={t("sessions.actions", {title: sessionTitle(session)})}><MoreHorizontalIcon /></Button>} />
      </li>)}</ul>}
      {!list.loading && !list.failed && !list.entries.length && !list.hasMore && <Empty><EmptyHeader><EmptyMedia variant="icon"><MessagesSquareIcon /></EmptyMedia><EmptyTitle>{t(archived ? "sessions.no_archived" : "sessions.empty")}</EmptyTitle></EmptyHeader></Empty>}
      {list.failed && <div className="dl-client-error" role="alert"><span>{t("lazy_page.load_failed")}</span><Button variant="ghost" size="sm" onClick={list.retry}>{t("app.retry")}</Button></div>}
      {!list.loading && list.hasMore && <LazyPage key={String(archived)} cursor={list.cursor} onLoad={list.more} errorMessage={t("lazy_page.load_failed")} />}
      {list.unavailable > 0 && <div className="dl-client-error" role="status"><span>{t("sessions.unavailable", {count: list.unavailable})}</span><Button variant="ghost" size="sm" onClick={list.retry}>{t("app.retry")}</Button></div>}
    </div>
  </section>;
}
