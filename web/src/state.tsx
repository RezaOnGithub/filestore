import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { api } from "./api";
import type { Operation, ServerStatus, Target } from "./types";
import { plural } from "./util";

export type Tab = "browse" | "suggestions" | "scans" | "blobs" | "lineages" | "tags" | "history" | "settings";

export interface Nav {
  tab: Tab;
  /** Directory shown in Browse. */
  path: string;
  /** Selected entry (path) in Browse. */
  sel: string | null;
  blob: string | null;
  lineage: string | null;
  scan: number | null;
  tag: string | null;
}

const DEFAULT_NAV: Nav = { tab: "browse", path: "", sel: null, blob: null, lineage: null, scan: null, tag: null };

function readHash(): Nav {
  const p = new URLSearchParams(location.hash.slice(1));
  const nav = { ...DEFAULT_NAV };
  const tab = p.get("tab");
  if (tab) nav.tab = tab as Tab;
  nav.path = p.get("path") ?? "";
  nav.sel = p.get("sel");
  nav.blob = p.get("blob");
  nav.lineage = p.get("lineage");
  nav.tag = p.get("tag");
  const scan = p.get("scan");
  nav.scan = scan ? Number(scan) : null;
  return nav;
}

function writeHash(nav: Nav) {
  const p = new URLSearchParams();
  p.set("tab", nav.tab);
  if (nav.path) p.set("path", nav.path);
  if (nav.sel != null) p.set("sel", nav.sel);
  if (nav.blob) p.set("blob", nav.blob);
  if (nav.lineage) p.set("lineage", nav.lineage);
  if (nav.tag) p.set("tag", nav.tag);
  if (nav.scan != null) p.set("scan", String(nav.scan));
  const h = `#${p.toString()}`;
  if (location.hash !== h) history.pushState(null, "", h);
}

export interface Toast {
  id: number;
  text: string;
  kind: "info" | "error";
}

interface AppCtx {
  status: ServerStatus | null;
  version: number;
  busy: boolean;
  nav: Nav;
  go(n: Partial<Nav>): void;
  toast(text: string, kind?: "info" | "error"): void;
  pollNow(): void;
  runScan(operation: Operation, targets: Target[]): Promise<void>;
  toasts: Toast[];
}

const Ctx = createContext<AppCtx | null>(null);

export function useApp(): AppCtx {
  const c = useContext(Ctx);
  if (!c) throw new Error("no app context");
  return c;
}

export function AppProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<ServerStatus | null>(null);
  const [nav, setNav] = useState<Nav>(readHash);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const timer = useRef<number | undefined>(undefined);
  const nextToast = useRef(1);

  const toast = useCallback((text: string, kind: "info" | "error" = "info") => {
    const id = nextToast.current++;
    setToasts((t) => [...t, { id, text, kind }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "error" ? 9000 : 5000);
  }, []);

  const poll = useCallback(async () => {
    window.clearTimeout(timer.current);
    let busy = false;
    try {
      const s = await api.status();
      busy = !!s.busy;
      setStatus(s);
    } catch {
      setStatus(null);
    }
    timer.current = window.setTimeout(poll, busy ? 400 : 1500);
  }, []);

  useEffect(() => {
    poll();
    const onHash = () => setNav(readHash());
    window.addEventListener("popstate", onHash);
    return () => {
      window.clearTimeout(timer.current);
      window.removeEventListener("popstate", onHash);
    };
  }, [poll]);

  const go = useCallback((n: Partial<Nav>) => {
    setNav((cur) => {
      const next = { ...cur, ...n };
      writeHash(next);
      return next;
    });
  }, []);

  const runScan = useCallback(
    async (operation: Operation, targets: Target[]) => {
      if (targets.length === 0) return;
      const waiting = status?.busy ? ` (waits for scan #${status.busy.scan_id})` : "";
      toast(`Requesting ${operation} of ${plural(targets.length, "target")}${waiting}…`);
      try {
        const r = await api.submitScan(operation, targets);
        toast(`Scan #${r.id} (${operation}) queued`);
      } catch (e) {
        toast(`Could not queue the scan: ${(e as Error).message}`, "error");
      }
      poll();
    },
    [status, toast, poll],
  );

  const value: AppCtx = {
    status,
    version: status?.data_version ?? 0,
    busy: !!status?.busy,
    nav,
    go,
    toast,
    pollNow: poll,
    runScan,
    toasts,
  };
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

/**
 * Fetch data for a view. Refetches when `deps` change, and when catalog data
 * changes (data_version) while no scan holds the catalog; during a scan the
 * last loaded data stays on screen with its original age.
 */
export function useApi<T>(fn: () => Promise<T>, deps: unknown[]) {
  const { version, busy } = useApp();
  const [state, setState] = useState<{ data?: T; error: string | null; loading: boolean }>({
    error: null,
    loading: true,
  });
  const [tick, setTick] = useState(0);
  const [seen, setSeen] = useState(version);
  useEffect(() => {
    if (!busy && version !== seen) setSeen(version);
  }, [version, busy, seen]);
  useEffect(() => {
    let cancelled = false;
    setState((s) => ({ ...s, loading: true }));
    fn().then(
      (data) => !cancelled && setState({ data, error: null, loading: false }),
      (e) => !cancelled && setState((s) => ({ data: s.data, error: String(e?.message ?? e), loading: false })),
    );
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, seen, tick]);
  return { ...state, reload: () => setTick((t) => t + 1) };
}
