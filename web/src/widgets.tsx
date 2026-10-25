import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { useApp } from "./state";
import type { Assignment, Status } from "./types";
import { STATUS_HELP, STATUS_LABEL, ago, displayPath, fmtTime, parentPath, shortId } from "./util";

export function Badge({ status, issue }: { status: Status; issue?: boolean }) {
  return (
    <span className={`badge st-${status}`} title={STATUS_HELP[status]}>
      {STATUS_LABEL[status] ?? status}
      {issue && <span className="badge-issue" title="The last attempt failed; this is last-known evidence">!</span>}
    </span>
  );
}

/** Relative time that stays current; the absolute time is in the tooltip. */
export function Ago({ ms }: { ms: number | null | undefined }) {
  const [, setNow] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setNow((n) => n + 1), 30_000);
    return () => clearInterval(t);
  }, []);
  if (ms == null) return <span className="muted">—</span>;
  return <span title={fmtTime(ms)}>{ago(ms)}</span>;
}

export function BlobLink({ id, full }: { id: string | null | undefined; full?: boolean }) {
  const { go } = useApp();
  if (!id) return <span className="muted">—</span>;
  return (
    <a
      className="mono blob-link"
      href="#"
      title={id}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        go({ tab: "blobs", blob: id });
      }}
    >
      {full ? id : shortId(id)}
    </a>
  );
}

/** Link that opens Browse at a path's directory with the entry selected. */
export function PathLink({ path, children }: { path: string; children?: ReactNode }) {
  const { go } = useApp();
  return (
    <a
      href="#"
      className="path-link"
      title={displayPath(path)}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        go({ tab: "browse", path: parentPath(path), sel: path });
      }}
    >
      {children ?? displayPath(path)}
    </a>
  );
}

export function TagChips({ tags, dim, onRemove }: { tags: Assignment[]; dim?: boolean; onRemove?: (a: Assignment) => void }) {
  if (!tags.length) return null;
  return (
    <span className={dim ? "tags dim" : "tags"} title={dim ? "Tags of the expected content; this file's content is not verified" : undefined}>
      {tags.map((t) => (
        <span key={t.hierarchy} className="tag" title={`${t.hierarchy_name}: ${t.tag_path}`}>
          <span className="tag-h">{t.hierarchy_name}</span>
          {t.tag_path}
          {onRemove && (
            <button className="tag-x" title="Remove this tag" onClick={() => onRemove(t)}>
              ×
            </button>
          )}
        </span>
      ))}
    </span>
  );
}

export function Modal({ title, onClose, children, wide }: { title: string; onClose(): void; children: ReactNode; wide?: boolean }) {
  return (
    <div className="modal-back" onMouseDown={onClose}>
      <div className={wide ? "modal wide" : "modal"} onMouseDown={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <b>{title}</b>
          <button className="icon" onClick={onClose} title="Close">
            ×
          </button>
        </div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  );
}

export function ErrorBox({ error }: { error: string | null | undefined }) {
  if (!error) return null;
  return <div className="error-box">{error}</div>;
}

export function Loading({ what }: { what?: string }) {
  const { busy, status } = useApp();
  return (
    <div className="muted pad">
      {busy
        ? `Waiting for scan #${status?.busy?.scan_id} to release the catalog${what ? ` (${what})` : ""}…`
        : `Loading${what ? ` ${what}` : ""}…`}
    </div>
  );
}

export function Outcome({ outcome, state }: { outcome: string | null; state?: string }) {
  const label = outcome ?? state ?? "";
  return <span className={`outcome oc-${label}`}>{label.replaceAll("_", " ")}</span>;
}
