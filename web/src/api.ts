import type {
  BlobDetail,
  BlobRow,
  DirView,
  Entry,
  Hierarchy,
  LineageSummary,
  LineageView,
  LocationDetail,
  OpView,
  Operation,
  ScanDetail,
  ScanView,
  ServerStatus,
  Suggestion,
  TagTargets,
  Target,
  TreeNode,
} from "./types";

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function request<T>(method: string, url: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method, headers: {} };
  if (body !== undefined) {
    init.body = JSON.stringify(body);
    (init.headers as Record<string, string>)["content-type"] = "application/json";
  }
  let res: Response;
  try {
    res = await fetch(url, init);
  } catch {
    // One retry on a network failure. Mutations carry a request ID, so a
    // retry of a committed command returns the original result.
    res = await fetch(url, init);
  }
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!res.ok) throw new ApiError(res.status, data?.error ?? res.statusText);
  return data as T;
}

const q = (params: Record<string, string | boolean | undefined>) =>
  new URLSearchParams(
    Object.entries(params)
      .filter(([, v]) => v !== undefined && v !== false)
      .map(([k, v]) => [k, String(v)]),
  ).toString();

const rid = () => crypto.randomUUID();

export const api = {
  status: () => request<ServerStatus>("GET", "/api/status"),
  tree: (path: string) => request<TreeNode[]>("GET", `/api/tree?${q({ path })}`),
  dir: (path: string, history: boolean) => request<DirView>("GET", `/api/dir?${q({ path, history })}`),
  location: (path: string) => request<LocationDetail>("GET", `/api/location?${q({ path })}`),
  search: (text: string) => request<Entry[]>("GET", `/api/search?${q({ q: text })}`),
  blobs: (filter: string, text: string) => request<BlobRow[]>("GET", `/api/blobs?${q({ filter, q: text || undefined })}`),
  blob: (id: string) => request<BlobDetail>("GET", `/api/blobs/${id}`),
  scans: () => request<ScanView[]>("GET", "/api/scans"),
  scan: (id: number) => request<ScanDetail>("GET", `/api/scans/${id}`),
  submitScan: (operation: Operation, targets: Target[]) =>
    request<{ id: number; public_id: string }>("POST", "/api/scans", { operation, targets, request_id: rid() }),
  cancelScan: (id: number) => request<{ cancelled: string }>("POST", `/api/scans/${id}/cancel`),
  suggestions: () => request<Suggestion[]>("GET", "/api/suggestions"),
  acceptSuggestion: (id: string) =>
    request<{ id: number; public_id: string }>("POST", `/api/suggestions/${id}/accept`, { request_id: rid() }),
  tags: () => request<Hierarchy[]>("GET", "/api/tags"),
  tagTargets: (id: string) => request<TagTargets>("GET", `/api/tags/${id}`),
  createHierarchy: (name: string) => request<{ id: string }>("POST", "/api/tag-hierarchies", { name }),
  createTag: (hierarchy: string, parent: string | null, name: string) =>
    request<{ id: string }>("POST", "/api/tags", { hierarchy, parent, name }),
  renameTag: (id: string, name: string) => request<object>("PATCH", `/api/tags/${id}`, { name }),
  assign: (target_kind: "blob" | "lineage" | "revision", target: string, hierarchy: string, tag: string | null) =>
    request<object>("POST", "/api/assignments", { target_kind, target, hierarchy, tag }),
  lineages: () => request<LineageSummary[]>("GET", "/api/lineages"),
  lineage: (id: string) => request<LineageView>("GET", `/api/lineages/${id}`),
  createLineage: (
    title: string,
    revisions: { blob: string; label: string | null; parents: (number | string)[] }[],
    suggestion?: string,
    modifications?: unknown,
  ) =>
    request<{ id: string }>("POST", "/api/lineages", { title, revisions, suggestion, modifications, request_id: rid() }),
  renameLineage: (id: string, title: string) => request<object>("PATCH", `/api/lineages/${id}`, { title }),
  removeLineage: (id: string) => request<object>("DELETE", `/api/lineages/${id}`),
  addRevision: (lineage: string, blob: string, label: string | null, parents: string[], suggestion?: string) =>
    request<{ id: string }>("POST", `/api/lineages/${lineage}/revisions`, {
      blob,
      label,
      parents,
      suggestion,
      request_id: rid(),
    }),
  updateRevision: (id: string, patch: { label?: string | null; parents?: string[] }) =>
    request<object>("PATCH", `/api/revisions/${id}`, patch),
  removeRevision: (id: string) => request<object>("DELETE", `/api/revisions/${id}`),
  history: () => request<OpView[]>("GET", "/api/history"),
  adoptRoot: () => request<{ epoch: string }>("POST", "/api/root/adopt"),
  setHorizon: (days: number) => request<object>("PUT", "/api/settings", { verify_horizon_days: days }),
};
