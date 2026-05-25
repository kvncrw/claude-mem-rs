"use client";

import {
  Activity,
  Box,
  Database,
  Download,
  FileText,
  GitBranch,
  HeartPulse,
  ListChecks,
  Logs,
  Moon,
  Play,
  RotateCw,
  Save,
  Search,
  Settings,
  Sun,
  X,
} from "lucide-react";
import { FormEvent, KeyboardEvent, ReactNode, useEffect, useMemo, useState } from "react";

type Counts = {
  sessions?: number;
  observations?: number;
  summaries?: number;
  prompts?: number;
  pendingMessages?: number;
};

type ActivityWindow = {
  observations15m?: number;
  summaries15m?: number;
  prompts15m?: number;
  sessions15m?: number;
  latestObservationEpoch?: number;
  latestSummaryEpoch?: number;
  latestPromptEpoch?: number;
  latestSessionEpoch?: number;
};

type Doctor = {
  ok?: boolean;
  mcpReady?: boolean;
  platform?: string;
  pid?: number;
  counts?: Counts;
  activity?: ActivityWindow;
  qdrant?: { compiled?: boolean; enabled?: boolean };
};

type Project = { project: string; observationCount?: number; latestEpoch?: number };
type QueueMessage = { id?: number; messageId?: number; messageType?: string; status?: string };
type QueueState = {
  queue?: {
    messages?: QueueMessage[];
    totalPending?: number;
    totalProcessing?: number;
    totalFailed?: number;
    stuckCount?: number;
  };
  recentlyProcessed?: QueueMessage[];
  recentlyCompleted?: {
    window?: string;
    windowLabel?: string;
    processed?: number;
    failed?: number;
    total?: number;
    observations?: number;
    summaries?: number;
    windowMs?: number;
  };
  activityWindow?: {
    window?: string;
    windowLabel?: string;
    observations?: number;
    summaries?: number;
    prompts?: number;
    sessions?: number;
    total?: number;
  };
  tokenMetrics?: {
    window?: string;
    windowLabel?: string;
    inputTokens?: number;
    outputTokens?: number;
    totalTokens?: number;
    estimatedCostUsd?: number;
    source?: string;
  };
};
type ProcessingState = { pending?: number; processing?: number; active?: boolean };
type MemoryItem = {
  id: number;
  project?: string;
  type?: string;
  title?: string;
  request?: string;
  narrative?: string;
  learned?: string;
  completed?: string;
  facts?: string[];
  created_at_epoch?: number;
  platform_source?: string;
  content_session_id?: string;
};
type EventItem = { name: string; data: unknown; at: number };
type MetricWindow = "15m" | "24h" | "all";

const TYPES = ["discovery", "decision", "implementation", "bugfix", "refactor", "constraint"];
const WINDOWS: { id: MetricWindow; label: string }[] = [
  { id: "15m", label: "15m" },
  { id: "24h", label: "24h" },
  { id: "all", label: "All" },
];

async function api<T>(path: string, options?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    headers: { "content-type": "application/json" },
    ...options,
  });
  const text = await response.text();
  let body: unknown = text;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = text;
  }
  if (!response.ok) {
    const message =
      typeof body === "string"
        ? body
        : body && typeof body === "object" && "error" in body
          ? String((body as { error: unknown }).error)
          : response.statusText;
    throw new Error(message);
  }
  return body as T;
}

function fmt(epoch?: number) {
  if (!epoch) return "";
  const millis = epoch < 1e12 ? epoch * 1000 : epoch;
  return new Date(millis).toLocaleString();
}

function age(epoch?: number) {
  if (!epoch) return "never";
  const millis = epoch < 1e12 ? epoch * 1000 : epoch;
  const seconds = Math.max(0, Math.floor((Date.now() - millis) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function clip(value?: unknown, length = 420) {
  const text = String(value ?? "");
  return text.length > length ? `${text.slice(0, length - 3)}...` : text;
}

function eventSummary(data: unknown) {
  return clip(JSON.stringify(data), 360);
}

function compactNumber(value?: number) {
  const num = Number(value || 0);
  if (num >= 1_000_000) return `${(num / 1_000_000).toFixed(num >= 10_000_000 ? 0 : 1)}m`;
  if (num >= 1_000) return `${(num / 1_000).toFixed(num >= 10_000 ? 0 : 1)}k`;
  return String(num);
}

function money(value?: number) {
  const num = Number(value || 0);
  if (num > 0 && num < 0.01) return `$${num.toFixed(4)}`;
  return `$${num.toFixed(2)}`;
}

export default function Dashboard() {
  const [theme, setTheme] = useState("light");
  const [metricWindow, setMetricWindow] = useState<MetricWindow>("15m");
  const [settingsExpanded, setSettingsExpanded] = useState(true);
  const [tab, setTab] = useState("feed");
  const [query, setQuery] = useState("");
  const [project, setProject] = useState("");
  const [type, setType] = useState("");
  const [doctor, setDoctor] = useState<Doctor>({});
  const [projects, setProjects] = useState<Project[]>([]);
  const [observations, setObservations] = useState<MemoryItem[]>([]);
  const [summaries, setSummaries] = useState<MemoryItem[]>([]);
  const [queue, setQueue] = useState<QueueState>({});
  const [processing, setProcessing] = useState<ProcessingState>({});
  const [events, setEvents] = useState<EventItem[]>([]);
  const [connected, setConnected] = useState(false);
  const [searchOut, setSearchOut] = useState("Run a search from the top bar.");
  const [timelineOut, setTimelineOut] = useState("Select a memory card to inspect local timeline.");
  const [contextProject, setContextProject] = useState("");
  const [contextOut, setContextOut] = useState("");
  const [adminOut, setAdminOut] = useState("");
  const [drawer, setDrawer] = useState<{ title: string; body: string } | null>(null);
  const [drawerError, setDrawerError] = useState("");
  const [settingsDraft, setSettingsDraft] = useState("");
  const [logsDraft, setLogsDraft] = useState("");

  useEffect(() => {
    const saved = localStorage.getItem("cmemTheme") || "light";
    setTheme(saved);
    document.documentElement.dataset.theme = saved;
    const savedWindow = localStorage.getItem("cmemMetricWindow") as MetricWindow | null;
    if (savedWindow && WINDOWS.some((item) => item.id === savedWindow)) {
      setMetricWindow(savedWindow);
    }
  }, []);

  useEffect(() => {
    if (!settingsExpanded) return;
    const timer = window.setTimeout(() => setSettingsExpanded(false), 30_000);
    return () => window.clearTimeout(timer);
  }, [settingsExpanded, metricWindow]);

  async function refreshAll() {
    const [doctorData, projectData, obsData, summaryData, queueData, processingData] =
      await Promise.all([
        api<Doctor>("/api/admin/doctor"),
        api<{ projects: Project[] }>("/api/projects"),
        api<{ observations: MemoryItem[] }>("/api/observations?limit=40"),
        api<{ summaries: MemoryItem[] }>("/api/summaries?limit=40"),
        api<QueueState>(`/api/pending-queue?window=${metricWindow}`),
        api<ProcessingState>("/api/processing-status"),
      ]);
    setDoctor(doctorData);
    setProjects(projectData.projects || []);
    setObservations(obsData.observations || []);
    setSummaries(summaryData.summaries || []);
    setQueue(queueData);
    setProcessing(processingData);
    setConnected(true);
  }

  useEffect(() => {
    let refreshTimer: number | undefined;
    const scheduleRefresh = () => {
      window.clearTimeout(refreshTimer);
      refreshTimer = window.setTimeout(() => {
        refreshAll().catch((error) => pushEvent("refresh_error", { error: error.message }));
      }, 650);
    };
    const pushEvent = (name: string, data: unknown) => {
      setEvents((current) => [{ name, data, at: Date.now() }, ...current].slice(0, 60));
    };

    refreshAll().catch((error) => {
      setConnected(false);
      pushEvent("refresh_error", { error: error.message });
    });

    const source = new EventSource("/stream");
    source.onopen = () => setConnected(true);
    source.onerror = () => setConnected(false);
    for (const name of [
      "initial_load",
      "memory_saved",
      "session_initialized",
      "session_completed",
      "observation_processed",
      "summary_processed",
      "summary_stored",
      "queue_processed",
      "stream_lagged",
    ]) {
      source.addEventListener(name, (event) => {
        const data = JSON.parse(event.data);
        pushEvent(name, data);
        if (name === "initial_load") {
          if (Array.isArray(data.observations)) setObservations(data.observations);
          if (Array.isArray(data.summaries)) setSummaries(data.summaries);
        }
        scheduleRefresh();
      });
    }
    return () => {
      window.clearTimeout(refreshTimer);
      source.close();
    };
  }, [metricWindow]);

  const counts = doctor.counts || {};
  const activity = doctor.activity || {};
  const queueTotals = queue.queue || {};
  const queueRecent = queue.recentlyCompleted || {};
  const activityWindow = queue.activityWindow || {};
  const tokenMetrics = queue.tokenMetrics || {};
  const windowLabel = queueRecent.windowLabel || activityWindow.windowLabel || "last 15m";
  const recent =
    activityWindow.total ??
    (activity.observations15m || 0) + (activity.summaries15m || 0) + (activity.prompts15m || 0);

  const feed = useMemo(() => {
    const allowed = (item: MemoryItem) =>
      (!project || item.project === project) && (!type || item.type === type);
    return [
      ...observations.filter(allowed).map((item) => ({ kind: "observation", item })),
      ...summaries.filter(allowed).map((item) => ({ kind: "summary", item })),
    ]
      .sort((a, b) => (b.item.created_at_epoch || 0) - (a.item.created_at_epoch || 0))
      .slice(0, 80);
  }, [observations, project, summaries, type]);

  function toggleTheme() {
    const next = theme === "dark" ? "light" : "dark";
    setTheme(next);
    localStorage.setItem("cmemTheme", next);
    document.documentElement.dataset.theme = next;
  }

  function changeMetricWindow(next: MetricWindow) {
    setMetricWindow(next);
    localStorage.setItem("cmemMetricWindow", next);
    setSettingsExpanded(true);
  }

  async function runSearch() {
    if (!query.trim()) {
      setSearchOut("Run a search from the top bar.");
      return;
    }
    setTab("search");
    const params = new URLSearchParams({ query, limit: "25", format: "text" });
    if (project) params.set("project", project);
    const result = await api<{ content?: { text?: string }[] }>(`/api/search?${params}`);
    setSearchOut(result.content?.[0]?.text || JSON.stringify(result, null, 2));
  }

  function onQueryKey(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter") {
      runSearch().catch((error) => setSearchOut(error.message));
    }
  }

  async function loadTimeline(id: number) {
    setTab("timeline");
    const result = await api<unknown>(`/api/timeline?anchor=${encodeURIComponent(id)}&depth_before=4&depth_after=4`);
    setTimelineOut(JSON.stringify(result, null, 2));
  }

  async function processQueue() {
    const result = await api<unknown>("/api/pending-queue/process", { method: "POST", body: "{}" });
    setEvents((current) => [{ name: "queue_processed", data: result, at: Date.now() }, ...current].slice(0, 60));
    await refreshAll();
  }

  async function openSettings() {
    const result = await api<unknown>("/api/settings");
    setDrawerError("");
    setSettingsDraft(JSON.stringify(result, null, 2));
    setDrawer({ title: "Settings", body: "__settings_form__" });
  }

  async function openLogs() {
    const result = await api<unknown>("/api/logs?limit=200");
    setDrawerError("");
    setLogsDraft(JSON.stringify(result, null, 2));
    setDrawer({ title: "Logs", body: "__logs_view__" });
  }

  async function openExport() {
    const result = await api<unknown>("/api/export");
    setDrawer({ title: "Export JSON", body: JSON.stringify(result, null, 2) });
  }

  async function saveManualMemory(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const formData = new FormData(event.currentTarget);
    const body = {
      project: String(formData.get("project") || "manual"),
      title: String(formData.get("title") || "Manual memory"),
      text: String(formData.get("text") || ""),
    };
    await api("/api/memory/save", { method: "POST", body: JSON.stringify(body) });
    setDrawer(null);
    await refreshAll();
  }

  async function saveSettings() {
    try {
      JSON.parse(settingsDraft);
      await api("/api/settings", { method: "POST", body: settingsDraft });
      setDrawer(null);
      setDrawerError("");
      await refreshAll();
    } catch (error) {
      setDrawerError(error instanceof Error ? error.message : String(error));
    }
  }

  async function clearLogs() {
    try {
      await api("/api/logs/clear", { method: "POST", body: "{}" });
      const result = await api<unknown>("/api/logs?limit=200");
      setLogsDraft(JSON.stringify(result, null, 2));
      setDrawerError("");
    } catch (error) {
      setDrawerError(error instanceof Error ? error.message : String(error));
    }
  }

  async function loadContextPreview() {
    if (!contextProject) return;
    const response = await fetch(`/api/context/inject?project=${encodeURIComponent(contextProject)}`);
    setContextOut(await response.text());
  }

  async function loadDoctor() {
    const result = await api<unknown>("/api/admin/doctor");
    setAdminOut(JSON.stringify(result, null, 2));
  }

  async function loadBranch() {
    const result = await api<unknown>("/api/branch/status");
    setAdminOut(JSON.stringify(result, null, 2));
  }

  return (
    <>
      <header>
        <div className="topbar">
          <div className="brand">
            <div className="mark">CM</div>
            <div>
              <h1>claude-mem-rs</h1>
              <span>native Rust memory runtime</span>
            </div>
          </div>
          <div className="filters">
            <div className="searchBox">
              <Search size={16} />
              <input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={onQueryKey}
                placeholder="Search memories, files, concepts"
              />
            </div>
            <select value={project} onChange={(event) => setProject(event.target.value)}>
              <option value="">All projects</option>
              {projects.map((item) => (
                <option key={item.project} value={item.project}>
                  {item.project}
                </option>
              ))}
            </select>
            <select value={type} onChange={(event) => setType(event.target.value)}>
              <option value="">All types</option>
              {TYPES.map((item) => (
                <option key={item} value={item}>
                  {item}
                </option>
              ))}
            </select>
          </div>
          <div className="actions">
            <span className="pill">
              <span className={`dot ${connected ? "ok" : ""}`} />
              {connected ? "live" : "offline"}
            </span>
            <button onClick={toggleTheme} title="Toggle theme">
              {theme === "dark" ? <Sun size={16} /> : <Moon size={16} />}
            </button>
          </div>
        </div>
      </header>

      <main>
        <aside className="panel">
          <Section title="Operational State">
            <div className="stats">
              <Stat label="worker" value={doctor.ok ? "healthy" : "down"} tone={doctor.ok ? "ok" : "bad"} />
              <Stat label={`memory formed ${windowLabel}`} value={recent} tone={recent ? "ok" : "warn"} />
              <Stat
                label="backlog p/r/f"
                value={`${queueTotals.totalPending || 0}/${queueTotals.totalProcessing || processing.processing || 0}/${queueTotals.totalFailed || 0}`}
                tone={queueTotals.stuckCount || queueTotals.totalFailed ? "bad" : "ok"}
              />
              <Stat
                label={`queue done ${windowLabel}`}
                value={`${queueRecent.processed || 0}/${queueRecent.failed || 0}`}
                tone={queueRecent.failed ? "bad" : queueRecent.processed ? "ok" : "warn"}
              />
              <Stat
                label={`tokens in/out ${windowLabel}`}
                value={`${compactNumber(tokenMetrics.inputTokens)}/${compactNumber(tokenMetrics.outputTokens)}`}
                tone={tokenMetrics.totalTokens ? "ok" : "warn"}
              />
              <Stat label={`est cost ${windowLabel}`} value={money(tokenMetrics.estimatedCostUsd)} />
              <Stat label="indexed corpus" value={`${counts.observations || 0} obs`} tone={counts.observations ? "ok" : "warn"} />
              <Stat label="summaries" value={counts.summaries || 0} />
            </div>
            <div className="ops">
              <Op label="MCP tools" value={doctor.mcpReady ? "ready" : "not ready"} tone={doctor.mcpReady ? "ok" : "bad"} />
              <Op
                label="Queue model"
                value={
                  queueTotals.stuckCount
                    ? `${queueTotals.stuckCount} stuck`
                    : queueTotals.totalPending || queueTotals.totalProcessing || queueTotals.totalFailed
                      ? "active backlog"
                      : "caught up"
                }
              />
              <Op label="Search backend" value={doctor.qdrant?.enabled ? "qdrant" : "sqlite"} />
              <Op label="Qdrant build" value={doctor.qdrant?.compiled ? "compiled" : "not compiled"} />
              <Op label="Last observation" value={age(activity.latestObservationEpoch)} />
              <Op label="Last summary" value={age(activity.latestSummaryEpoch)} />
              <Op label="Last prompt" value={age(activity.latestPromptEpoch)} />
              <Op label="Last stream event" value={events[0] ? age(events[0].at) : "never"} />
            </div>
            <div className="buttonGrid">
              <button className="primary" onClick={() => setDrawer({ title: "Save Manual Memory", body: "__memory_form__" })}>
                <Save size={16} />
                Save
              </button>
              <button onClick={processQueue}>
                <Play size={16} />
                Queue
              </button>
              <button onClick={openExport}>
                <Download size={16} />
                Export
              </button>
            </div>
          </Section>

          <Section title="Projects">
            <div className="sideList">
              {projects.length ? (
                projects.map((item) => (
                  <button className="sideRow" key={item.project} onClick={() => setProject(item.project)}>
                    <span>{item.project}</span>
                    <strong>{item.observationCount || 0}</strong>
                  </button>
                ))
              ) : (
                <p className="muted">No projects</p>
              )}
            </div>
          </Section>

          <Section title="Queue">
            <strong>
              {queueTotals.totalPending || queueTotals.totalProcessing || queueTotals.totalFailed ? "Backlog active" : "Caught up"}
            </strong>
            <p className="muted small">
              pending {queueTotals.totalPending || 0}, processing {queueTotals.totalProcessing || 0}, failed{" "}
              {queueTotals.totalFailed || 0}, stuck {queueTotals.stuckCount || 0}. Completed {windowLabel}: {queueRecent.processed || 0} processed,{" "}
              {queueRecent.failed || 0} failed.
            </p>
            <p className="muted small">
              Recent mix: {queueRecent.observations || 0} observations, {queueRecent.summaries || 0} summaries.
            </p>
            <p className="muted small">
              Tokens {windowLabel}: {compactNumber(tokenMetrics.inputTokens)} in,{" "}
              {compactNumber(tokenMetrics.outputTokens)} out, {money(tokenMetrics.estimatedCostUsd)} est.
            </p>
            <div className="sideList">
              {queueTotals.messages?.length ? (
                queueTotals.messages.slice(0, 20).map((message) => (
                  <div className="sideRow" key={message.id || message.messageId}>
                    <span>
                      #{message.id || message.messageId} {message.messageType || "message"}
                    </span>
                    <span>{message.status}</span>
                  </div>
                ))
              ) : queue.recentlyProcessed?.length ? (
                queue.recentlyProcessed.slice(0, 8).map((message) => (
                  <div className="sideRow" key={message.messageId}>
                    <span>
                      #{message.messageId} {message.messageType || "message"}
                    </span>
                    <span>{message.status}</span>
                  </div>
                ))
              ) : (
                <p className="muted">No persistent backlog.</p>
              )}
            </div>
          </Section>
        </aside>

        <section className="panel work">
          <nav className="tabs">
            {[
              ["feed", "Feed", Activity],
              ["search", "Search", Search],
              ["timeline", "Timeline", ListChecks],
              ["admin", "Admin", HeartPulse],
            ].map(([id, label, Icon]) => (
              <button key={String(id)} className={tab === id ? "active" : ""} onClick={() => setTab(String(id))}>
                <Icon size={15} />
                {String(label)}
              </button>
            ))}
          </nav>
          <div className="panelBody">
            {tab === "feed" && (
              <div className="feed">
                {feed.length ? (
                  feed.map(({ kind, item }) => (
                    <article className="memoryCard" key={`${kind}-${item.id}`}>
                      <div className="cardHead">
                        <div>
                          <h3>{clip(item.title || item.request || item.content_session_id || `${kind} #${item.id}`, 120)}</h3>
                          <div className="meta">
                            <span>{kind}</span>
                            <span>{item.project}</span>
                            <span>{fmt(item.created_at_epoch)}</span>
                            <span>{item.type || item.platform_source}</span>
                          </div>
                        </div>
                        <button onClick={() => loadTimeline(item.id)}>
                          <GitBranch size={15} />
                        </button>
                      </div>
                      <p>{clip(item.narrative || item.learned || item.completed, 520)}</p>
                      {item.facts?.length ? (
                        <ul>
                          {item.facts.slice(0, 5).map((fact) => (
                            <li key={fact}>{clip(fact, 180)}</li>
                          ))}
                        </ul>
                      ) : null}
                    </article>
                  ))
                ) : (
                  <div className="empty">No memories loaded.</div>
                )}
              </div>
            )}
            {tab === "search" && <pre>{searchOut}</pre>}
            {tab === "timeline" && <pre>{timelineOut}</pre>}
            {tab === "admin" && (
              <div className="grid">
                <div className="split">
                  <button onClick={loadDoctor}>
                    <HeartPulse size={16} />
                    Doctor
                  </button>
                  <button onClick={loadBranch}>
                    <GitBranch size={16} />
                    Branch
                  </button>
                </div>
                <pre>{adminOut}</pre>
              </div>
            )}
          </div>
        </section>

        <aside className="panel">
          <Section title="Settings">
            <button className="sectionToggle" onClick={() => setSettingsExpanded((value) => !value)}>
              <Settings size={16} />
              {settingsExpanded ? "Collapse" : "Expand"}
            </button>
            {settingsExpanded ? (
              <div className="grid">
                <div>
                  <p className="muted small">Metric window</p>
                  <div className="segmented">
                    {WINDOWS.map((item) => (
                      <button
                        key={item.id}
                        className={metricWindow === item.id ? "active" : ""}
                        onClick={() => changeMetricWindow(item.id)}
                      >
                        {item.label}
                      </button>
                    ))}
                  </div>
                </div>
                <div className="split">
                  <button onClick={openSettings}>
                    <Settings size={16} />
                    JSON
                  </button>
                  <button onClick={openLogs}>
                    <Logs size={16} />
                    Logs
                  </button>
                </div>
              </div>
            ) : (
              <p className="muted small">Metric window: {windowLabel}</p>
            )}
          </Section>
          <Section title="Context Preview">
            <select value={contextProject} onChange={(event) => setContextProject(event.target.value)}>
              <option value="">Choose project</option>
              {projects.map((item) => (
                <option key={item.project} value={item.project}>
                  {item.project}
                </option>
              ))}
            </select>
            <button onClick={loadContextPreview}>
              <FileText size={16} />
              Load
            </button>
            <pre>{contextOut}</pre>
          </Section>
          <Section title="Live Events">
            <div className="eventList">
              {events.length ? (
                events.map((event, index) => (
                  <div className="event" key={`${event.name}-${event.at}-${index}`}>
                    <time>{new Date(event.at).toLocaleTimeString()}</time>
                    <strong>{event.name}</strong>
                    <p>{eventSummary(event.data)}</p>
                  </div>
                ))
              ) : (
                <p className="muted">Waiting for stream events.</p>
              )}
            </div>
          </Section>
        </aside>
      </main>

      {drawer ? (
        <div className="drawer">
          <div className="drawerCard">
            <div className="drawerHead">
              <strong>{drawer.title}</strong>
              <button onClick={() => setDrawer(null)}>
                <X size={16} />
              </button>
            </div>
            <div className="drawerBody">
              {drawerError ? <p className="errorText">{drawerError}</p> : null}
              {drawer.body === "__memory_form__" ? (
                <form onSubmit={saveManualMemory} className="grid">
                  <input name="project" placeholder="project" />
                  <input name="title" placeholder="title" />
                  <textarea name="text" placeholder="memory text" />
                  <button className="primary" type="submit">
                    <Save size={16} />
                    Save
                  </button>
                </form>
              ) : drawer.body === "__settings_form__" ? (
                <div className="grid">
                  <textarea
                    className="jsonEditor"
                    value={settingsDraft}
                    onChange={(event) => setSettingsDraft(event.target.value)}
                    spellCheck={false}
                  />
                  <div className="drawerActions">
                    <button className="primary" onClick={saveSettings}>
                      <Save size={16} />
                      Save Settings
                    </button>
                  </div>
                </div>
              ) : drawer.body === "__logs_view__" ? (
                <div className="grid">
                  <div className="drawerActions">
                    <button onClick={clearLogs}>
                      <X size={16} />
                      Clear Logs
                    </button>
                  </div>
                  <pre>{logsDraft}</pre>
                </div>
              ) : (
                <pre>{drawer.body}</pre>
              )}
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="section">
      <h2>{title}</h2>
      <div className="sectionBody">{children}</div>
    </section>
  );
}

function Stat({ label, value, tone = "" }: { label: string; value: string | number; tone?: string }) {
  return (
    <div className={`stat ${tone}`}>
      <strong>{value}</strong>
      <span>{label}</span>
    </div>
  );
}

function Op({ label, value, tone = "" }: { label: string; value: string; tone?: string }) {
  return (
    <div className="op">
      <span>{label}</span>
      <strong className={tone}>{value}</strong>
    </div>
  );
}
