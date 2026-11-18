// Settings: general, approval rules, review scope, reviewing, files, about. Plugin settings live under Plugins.
import { useState } from "preact/hooks";
import { api } from "../api";
import { getTheme, setTheme, useStore, type Theme } from "../app";
import { setLang, t } from "../i18n";
import { Card, Lines, Note, Row, Section, Segmented, Select, Switch, TextInput, Button } from "../ui";

export function SettingsPage() {
  const { draft, edit, ov, loaded, refresh, toast } = useStore();
  const [theme, setThemeState] = useState<Theme>(getTheme());
  // one group at a time: the page stays short and each group is one click away
  const [tab, setTab] = useState("general");
  const num = (path: (c: typeof draft) => number, set: (c: typeof draft, v: number) => void) =>
    <TextInput type="number" value={path(draft)} onInput={(v) => edit((c) => set(c, Number(v)))} />;
  return (
    <div class="page narrow tabbed">
      <header class="page-head"><h1>{t("set.title")}</h1></header>
      <Segmented value={tab} onChange={setTab} options={[
        ["general", t("set.general")], ["approve", t("set.approve")], ["scope", t("set.scope")],
        ["review", t("set.review")], ["about", t("set.about")],
      ]} />
      {loaded.error && (
        <Note tone="bad"><b>{t("set.broken")}</b><br />{t("set.broken_hint")}
          <details><summary>{t("status.detail")}</summary><code class="small">{loaded.error}</code></details></Note>
      )}

      {tab === "general" && <Section title={t("set.general")}>
        <Card>
          <Row label={t("set.language")} help={t("set.language_help")}>
            <Select value={draft.language} options={[["zh-CN", "简体中文"], ["en", "English"]]}
              onChange={(v) => { setLang(v); edit((c) => { c.language = v; }); }} />
          </Row>
          <Row label={t("set.theme")}>
            <Segmented value={theme} onChange={(v) => { setTheme(v as Theme); setThemeState(v as Theme); }}
              options={[["system", t("set.theme.system")], ["light", t("set.theme.light")], ["dark", t("set.theme.dark")]]} />
          </Row>
          <Row label={t("set.autostart")} help={t("set.autostart_help")}>
            <Switch on={ov.autostart} label={t("set.autostart")} onChange={async (v) => {
              try { await api.autostart(v); } catch (e) { toast(String(e), "bad"); }
              await refresh();
            }} />
          </Row>
        </Card>
      </Section>}

      {tab === "approve" && <Section title={t("set.approve")} help={t("set.approve_help")}>
        <Card>
          <Row label={t("set.approve.mode")}>
            <Segmented value={draft.approve.mode} onChange={(v) => edit((c) => { c.approve.mode = v; })}
              options={[["auto", t("set.approve.auto")], ["never", t("set.approve.never")]]} />
          </Row>
          {draft.approve.mode === "auto" && <>
            <Row label={t("set.approve.repos")} help={t("set.list_help")} wide>
              <Lines value={draft.approve.repos} onChange={(v) => edit((c) => { c.approve.repos = v; })} placeholder="my-org/*" /></Row>
            <Row label={t("set.approve.authors")} wide>
              <Lines value={draft.approve.authors} onChange={(v) => edit((c) => { c.approve.authors = v; })} placeholder="dependabot[bot]" /></Row>
            <Row label={t("set.approve.exclude_repos")} wide>
              <Lines value={draft.approve.exclude_repos} onChange={(v) => edit((c) => { c.approve.exclude_repos = v; })} /></Row>
            <Row label={t("set.approve.exclude_authors")} wide>
              <Lines value={draft.approve.exclude_authors} onChange={(v) => edit((c) => { c.approve.exclude_authors = v; })} /></Row>
          </>}
        </Card>
      </Section>}

      {tab === "scope" && <Section title={t("set.scope")}>
        <Card>
          <Row label={t("set.scope.repos")} help={t("set.list_help")} wide>
            <Lines value={draft.scope.repos} onChange={(v) => edit((c) => { c.scope.repos = v; })} placeholder="my-org/*" /></Row>
          <Row label={t("set.scope.exclude")} wide>
            <Lines value={draft.scope.exclude_repos} onChange={(v) => edit((c) => { c.scope.exclude_repos = v; })} /></Row>
          <Row label={t("set.scope.team")} help={t("set.scope.team_help")}>
            <Switch on={draft.scope.team_requests} label={t("set.scope.team")} onChange={(v) => edit((c) => { c.scope.team_requests = v; })} /></Row>
        </Card>
      </Section>}

      {tab === "review" && <Section title={t("set.review")}>
        <Card>
          <Row label={t("set.timeout")}>{num((c) => c.review.timeout_min, (c, v) => { c.review.timeout_min = v; })}</Row>
          <Row label={t("set.attempts")}>{num((c) => c.review.max_attempts, (c, v) => { c.review.max_attempts = v; })}</Row>
          <Row label={t("set.ci_wait")}>{num((c) => c.review.ci_wait_max_min, (c, v) => { c.review.ci_wait_max_min = v; })}</Row>
          <Row label={t("set.ci_grace")}>{num((c) => c.review.ci_none_grace_min, (c, v) => { c.review.ci_none_grace_min = v; })}</Row>
          <Row label={t("set.rereview")}>{num((c) => c.review.rereview_days, (c, v) => { c.review.rereview_days = v; })}</Row>
          <Row label={t("set.poll")}>{num((c) => c.daemon.poll_seconds, (c, v) => { c.daemon.poll_seconds = v; })}</Row>
          <Row label={t("set.refresh")}>{num((c) => c.daemon.refresh_seconds, (c, v) => { c.daemon.refresh_seconds = v; })}</Row>
        </Card>
      </Section>}

      {tab === "about" && <>
      <Section title={t("set.files")}>
        <Card class="row-buttons">
          <Button icon="folder" onClick={() => api.open("config")}>{t("set.open_config")}</Button>
          <Button icon="folder" onClick={() => api.open("data")}>{t("set.open_data")}</Button>
          <Button icon="file" onClick={() => api.open("log")}>{t("set.open_log")}</Button>
        </Card>
      </Section>

      <Section title={t("set.about")}>
        <Card><Row label="nod" help={t("set.version", { v: ov.version })}>
          <Button kind="quiet" icon="external" onClick={() => api.url("https://github.com/larrygogo/nod")}>GitHub</Button>
        </Row></Card>
      </Section>
      </>}
    </div>
  );
}
