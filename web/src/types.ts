// Shapes returned by the Filestore HTTP API (see crates/filestore/src/query.rs,
// advise.rs, semantic.rs). Paths are root-relative; "" is the root.

export type Operation = "inventory" | "verify" | "check";

export type FileStatus = "verified" | "modified" | "plausible" | "changed" | "unidentified";
export type DirStatus = "listed" | "stale" | "unlisted" | "boundary";
export type Status = FileStatus | DirStatus | "symlink" | "other" | "unknown" | "gone" | "previous_epoch" | "formerly_verified";

export interface Target {
  path: string;
  kind: "dir" | "file";
  depth?: number | null;
}

export interface Progress {
  scan_id: number;
  operation: Operation | null;
  started_at: number;
  dirs_listed: number;
  entries_seen: number;
  files_read: number;
  bytes_read: number;
  problems: number;
  current: string;
}

export interface EpochView {
  id: string;
  number: number;
  fs_context: string;
  root_ino: number;
  reason: string;
  created_at: number;
}

export interface CatalogInfo {
  catalog: string;
  root: string;
  archive_id: string;
  epoch: EpochView | null;
  epochs: number;
  publication_seq: number;
  pending_root_change: null | {
    reason: string;
    message: string;
    fs_context: string;
    root_ino: number;
    previous_fs_context: string;
    previous_root_ino: number;
    detected_at: number;
  };
  verify_horizon_days: number;
  scans: number;
  blobs: number;
  semantic_ops: number;
}

export interface RunOutcome {
  scan_id: number;
  outcome: string;
  publication_seq: number | null;
  error: string | null;
}

export interface ServerStatus {
  busy: Progress | null;
  queued: number[];
  data_version: number;
  info: CatalogInfo | null;
  last_scan: RunOutcome | null;
  recovered: number;
}

export interface Assignment {
  hierarchy: string;
  hierarchy_name: string;
  tag: string;
  tag_path: string;
}

export interface Counts {
  files: number;
  bytes: number;
  verified: number;
  modified: number;
  plausible: number;
  changed: number;
  unidentified: number;
  issues: number;
  overdue: number;
  dirs: number;
  dirs_unlisted: number;
  dirs_stale: number;
  dirs_issue: number;
  boundaries: number;
  other: number;
}

export interface Issue {
  code: string;
  detail: string | null;
  at: number | null;
}

export interface Entry {
  path: string;
  name: string;
  kind: "file" | "dir" | "symlink" | "other" | null;
  status: Status;
  present: boolean;
  epoch: number;
  ino: number | null;
  size: number | null;
  mtime_ms: number | null;
  first_seen_at: number;
  last_seen_at: number;
  last_seen_seq: number;
  ended_at: number | null;
  end_reason: string | null;
  boundary: boolean;
  meta_issue: string | null;
  blob: string | null;
  blob_size: number | null;
  content_at: number | null;
  modified: "size" | "mtime" | "ctime" | null;
  verified_blob: string | null;
  verified_at: number | null;
  overdue: boolean;
  check: { result: string; at: number; blob: string | null } | null;
  prev_blob: string | null;
  issue: Issue | null;
  listing: {
    listed_at: number | null;
    scan_seq: number | null;
    entries: number | null;
    stale: boolean;
    issue: Issue | null;
  } | null;
  format: string | null;
  tags: Assignment[];
  lineages: number;
  subtree: Counts | null;
}

export interface DirView {
  path: string;
  crumbs: { name: string; path: string }[];
  dir: Entry | null;
  previous: Entry | null;
  entries: Entry[];
  history: Entry[];
  truncated: boolean;
}

export interface TreeNode {
  name: string;
  path: string;
  status: Status;
  has_subdirs: boolean;
  counts: Counts;
}

export interface EventView {
  at: number;
  order: number;
  scan: number;
  operation: Operation;
  published: boolean;
  outcome: string | null;
  kind: "observation" | "listing" | "read";
  summary: string;
  ok: boolean | null;
}

export interface RevisionRef {
  revision: string;
  label: string | null;
  lineage: string;
  lineage_title: string;
}

export interface LocationDetail {
  path: string;
  current: Entry | null;
  occurrences: Entry[];
  events: EventView[];
  same_inode: { path: string; status: string; present: boolean; ended_at: number | null }[];
  revisions: RevisionRef[];
}

export interface BlobRow {
  id: string;
  size: number;
  created_at: number;
  format: string | null;
  present: number;
  paths: string[];
  tags: Assignment[];
  lineages: number;
}

export interface BlobDetail {
  id: string;
  size: number;
  created_at: number;
  admitted_scan: number;
  admitted_path: string;
  metadata: { extractor: string; version: string; config: string; payload: Record<string, unknown>; scan: number; created_at: number }[];
  baselines: string[];
  occurrences: {
    path: string;
    status: string;
    present: boolean;
    verified_at: number | null;
    content_at: number | null;
    ended_at: number | null;
    end_reason: string | null;
  }[];
  recognitions: number;
  tags: Assignment[];
  revisions: RevisionRef[];
}

export interface ScanView {
  id: number;
  public_id: string;
  operation: Operation;
  targets: Target[];
  state: "queued" | "running" | "closed";
  outcome: string | null;
  requested_at: number;
  started_at: number | null;
  finished_at: number | null;
  publication_seq: number | null;
  summary: null | {
    counts?: Record<string, number>;
    gaps?: number;
    gaps_by_code?: Record<string, number>;
    targets?: { path: string; kind: string; depth?: number; status: string; reason?: string }[];
    cancelled?: boolean;
    fatal?: string;
  };
  policy: Record<string, unknown> | null;
  error: string | null;
  scanner_version: string | null;
  advisor: string | null;
  suggestion: string | null;
}

export interface ScanDetail {
  scan: ScanView;
  diagnostics: { at: number; severity: string; code: string; message: string; path: string | null }[];
  diagnostics_total: number;
  proposal: Suggestion | null;
}

export interface ProposedRevision {
  blob: string;
  label: string | null;
  path: string;
}

export type Action =
  | { type: "scan"; operation: Operation; targets: Target[] }
  | { type: "create_lineage"; title: string; revisions: ProposedRevision[] }
  | {
      type: "add_revision";
      lineage: string;
      lineage_title: string;
      parent: string;
      parent_label: string | null;
      revision: ProposedRevision;
    };

export interface Suggestion {
  id: string;
  advisor: string;
  advisor_version: number;
  priority: number;
  title: string;
  reason: string;
  uncertainty: string;
  resolves: string;
  action: Action;
  evidence: { path: string; detail: string }[];
  evidence_total: number;
  input_seq: number;
}

export interface TagView {
  id: string;
  name: string;
  parent: string | null;
  path: string;
  usage: number;
}

export interface Hierarchy {
  id: string;
  name: string;
  tags: TagView[];
}

export interface TagTargets {
  tag: TagView;
  blobs: { id: string; size: number; tag_path: string; paths: string[] }[];
  lineages: { id: string; label: string; tag_path: string }[];
  revisions: { id: string; label: string; tag_path: string; lineage: string | null }[];
}

export interface LineageSummary {
  id: string;
  title: string;
  created_at: number;
  revisions: number;
  tags: Assignment[];
}

export interface RevisionView {
  id: string;
  label: string | null;
  blob: string;
  blob_size: number;
  format: string | null;
  created_at: number;
  removed: boolean;
  parents: string[];
  tags: Assignment[];
  paths: string[];
}

export interface LineageView {
  id: string;
  title: string;
  created_at: number;
  removed: boolean;
  tags: Assignment[];
  revisions: RevisionView[];
}

export interface OpView {
  id: string;
  kind: string;
  actor: string;
  summary: string;
  changes: unknown;
  created_at: number;
  advisor: string | null;
}
