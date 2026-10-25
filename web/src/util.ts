import type { Entry, Status } from "./types";

export function fmtBytes(n: number | null | undefined): string {
  if (n == null) return "";
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

export function fmtTime(ms: number | null | undefined): string {
  if (ms == null) return "";
  return new Date(ms).toLocaleString();
}

export function ago(ms: number | null | undefined, now = Date.now()): string {
  if (ms == null) return "never";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s} s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 48) return `${h} h ago`;
  const d = Math.round(h / 24);
  if (d < 60) return `${d} days ago`;
  const mo = Math.round(d / 30);
  if (mo < 24) return `${mo} months ago`;
  return `${Math.round(d / 365)} years ago`;
}

/** Display form of an API path (which is already lossless text). */
export function displayPath(path: string): string {
  return path === "" ? "/" : `/${path}`;
}

export function parentPath(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

export function baseName(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? path : path.slice(i + 1);
}

export function shortId(id: string | null | undefined): string {
  if (!id) return "";
  const i = id.indexOf("_");
  return i < 0 ? id.slice(0, 10) : id.slice(0, i + 9);
}

export const STATUS_LABEL: Record<Status, string> = {
  verified: "verified",
  modified: "modified?",
  plausible: "plausible",
  changed: "changed",
  unidentified: "unidentified",
  listed: "listed",
  stale: "stale listing",
  unlisted: "not listed",
  boundary: "mount boundary",
  symlink: "symlink",
  other: "special file",
  unknown: "unknown",
  gone: "gone",
  previous_epoch: "previous epoch",
  formerly_verified: "formerly",
};

export const STATUS_HELP: Record<Status, string> = {
  verified: "A full read recognized this content at this location, and nothing seen since contradicts it.",
  modified: "Content was established earlier, but the file's size or timestamps have changed since. Check or Verify.",
  plausible: "A sample check agreed with the expected content. Changes outside the sampled blocks would be missed.",
  changed: "A comparison ruled out the expected content here. The current content is unidentified.",
  unidentified: "Never fully read at this location. Its content is unknown.",
  listed: "Listed completely; unchanged since as far as later observations show.",
  stale: "The directory changed since its last complete listing; current entries are not established.",
  unlisted: "Seen but never listed: its entries are unknown, not empty.",
  boundary: "On another filesystem; not entered.",
  symlink: "Symbolic link; recorded, never followed.",
  other: "Device, FIFO, or socket; recorded, never read.",
  unknown: "Name seen, but its metadata could not be read.",
  gone: "No longer present at this path in the current view (history).",
  previous_epoch: "Seen in an earlier identity epoch; not established in the current filesystem context.",
  formerly_verified: "This location was verified with this content earlier; its content has since changed or been ruled out.",
};

export const END_REASON: Record<string, string> = {
  absent_in_listing: "a complete listing of its directory omitted it",
  ancestor_ended: "a parent directory was replaced or removed",
  replaced: "a different object (new inode) now occupies this path",
  type_changed: "the entry type changed",
  continuity_unresolved: "continuity with later observations could not be established",
};

const MODIFIED_FIELD = { size: "size", mtime: "modification time", ctime: "change time (permissions, links, …)" };

/** One-paragraph explanation of an entry's evidence, newest facts first. */
export function explain(e: Entry, now = Date.now()): string {
  const b = (id: string | null) => (id ? shortId(id) : "?");
  const parts: string[] = [];
  switch (e.status) {
    case "verified":
      parts.push(`Verified as ${b(e.blob)} by a full read ${ago(e.verified_at, now)}; metadata unchanged since.`);
      if (e.check && e.check.at > (e.verified_at ?? 0))
        parts.push(`Last sample check ${ago(e.check.at, now)}: ${e.check.result.replace("_", " ")}.`);
      if (e.overdue) parts.push("The verification is older than the verification horizon (overdue, not absent).");
      break;
    case "modified":
      parts.push(
        `Content was ${e.verified_blob === e.blob && e.verified_at === e.content_at ? "verified" : "plausibly"} ${b(e.blob)} as of ${ago(e.content_at, now)}, but its ${MODIFIED_FIELD[e.modified ?? "mtime"]} has changed since. Metadata alone does not prove a content change: Check or Verify.`,
      );
      break;
    case "plausible":
      parts.push(`A sample check ${ago(e.content_at, now)} matched ${b(e.blob)}.`);
      parts.push(
        e.verified_at
          ? `Last full verification here: ${ago(e.verified_at, now)}.`
          : "Never fully verified at this location (the match is against content previously seen at this path).",
      );
      parts.push("Changes outside the sampled blocks would not be detected.");
      break;
    case "changed":
      parts.push(
        `A ${e.check?.result === "size_mismatch" ? "size" : "sample"} comparison ${ago(e.content_at, now)} ruled out ${b(e.blob)} at this location. The current content is unidentified until Verify.`,
      );
      if (e.verified_blob) parts.push(`Earlier tags and revisions stay with ${b(e.verified_blob)}.`);
      break;
    case "unidentified":
      parts.push("Never fully read at this location; content unknown.");
      if (e.prev_blob) parts.push(`Earlier, ${b(e.prev_blob)} was verified at this path; Check compares against it.`);
      if (e.check?.result === "no_baseline") parts.push(`Last Check (${ago(e.check.at, now)}) had nothing to compare against.`);
      break;
    case "listed":
      parts.push(
        `Listed completely ${ago(e.listing?.listed_at, now)} (scan #${e.listing?.scan_seq ?? "?"}, ${e.listing?.entries ?? 0} entries); no change observed since.`,
      );
      break;
    case "stale":
      parts.push(`Listed ${ago(e.listing?.listed_at, now)}, but the directory has changed since; current entries are not established.`);
      break;
    case "gone":
      parts.push(`Not present in the current view since ${ago(e.ended_at, now)}: ${END_REASON[e.end_reason ?? ""] ?? e.end_reason}.`);
      parts.push("This does not prove its content was deleted rather than moved.");
      break;
    default:
      parts.push(STATUS_HELP[e.status]);
  }
  if (e.issue) parts.push(`Last attempt ${ago(e.issue.at, now)} failed (${e.issue.code}${e.issue.detail ? `: ${e.issue.detail}` : ""}); the evidence above is last-known.`);
  if (e.listing?.issue) parts.push(`Last listing attempt failed: ${e.listing.issue.code}.`);
  if (e.meta_issue) parts.push(`Metadata unavailable: ${e.meta_issue}.`);
  return parts.join(" ");
}

/** Whether an entry's content is exactly known (taggable, usable as a revision). */
export function exact(e: Entry): boolean {
  return e.status === "verified" && !!e.blob;
}

export function naturalCompare(a: string, b: string): number {
  return a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" });
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}
