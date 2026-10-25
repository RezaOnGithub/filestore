# Filestore: Ideas

This file collects proposals that are not yet part of [DESIGN.md](DESIGN.md) or
[the decision log](OPEN_QUESTIONS.md). An idea here is not an accepted
requirement. When one is adopted, record the decision and move its substance
into the design.

## `with-blob`: run an extractor on freshly verified Blob bytes

**Status:** Idea; not yet designed in detail.

**Motivation:** External utilities such as `file`, thumbnailers, and PDQ/vPDQ
hashers should operate on Blob IDs and use Filestore as their data source.
Filestore does not store the managed files, so such a tool must read an
occurrence that is believed to contain the Blob. Reading a path directly is
unsafe: its content may have changed since the last verification, and the result
would then be attached to the wrong Blob.
[Section 4.2](DESIGN.md#42-derived-metadata-and-extractors) already requires an
extractor to establish that its input bytes belong to the Blob it reports on.
Such a tool is an extractor, and its accepted output is derived metadata.

**Sketch:**

```text
filestore with-blob <blob-id> -- ffmpeg -i /dev/fd/3 -vf thumbnail -frames:v 1 out.jpg
```

1. Select a candidate location for the Blob, normally its last verified
   occurrence, and open it under the usual resolution rules.
2. Verify through that handle: full read, stability checks, and recognition of
   the requested Blob. A different or unrecognized Blob aborts without running
   the tool.
3. Keep the handle open and pass it to the tool as an inherited descriptor
   (`/dev/fd/3`). The descriptor refers to the regular file, so seeking tools
   work.
4. After the tool exits, check the handle again for identity, size, and change
   metadata.
5. Accept the tool's output only if recognition and the final stability check
   both pass. Record it with the read as its source.

Tools need no knowledge of Filestore: they read a descriptor and emit a result.
The verification is an ordinary Verify of a selected file, so it follows the
scan rules, including exclusive access, and contributes evidence as a side
effect. It accesses the drive and must be started explicitly, never implicitly
by the web UI.

**Relationship to Verify:**
[Section 4.2](DESIGN.md#42-derived-metadata-and-extractors) now specifies how
Verify runs extractors during a read: streaming extractors in the read loop, and
seeking extractors on the open handle after the hash. `with-blob` is that
seeking step run on its own, outside a scan the user asked for. It backfills
Blobs verified before a new extractor or configuration existed, and serves ad
hoc use. A backfill command such as
`extract --missing <extractor:config> <scope>` can apply it to selected Blobs.

**Open points:**

- Candidate selection when a Blob has several verified occurrences, and the
  fallback when the preferred one is unavailable or no longer matches.
- How a tool's output is submitted and validated when it does not run inside a
  Verify request, and how the run is recorded.
- Storage of large outputs such as thumbnails. One direction: small results go
  in the `blob_derived_metadata` payload, and large ones become files under the
  catalog directory, named by Blob ID, extractor, and configuration, with the
  record holding their size and digest.
- Cost on small files. Starting an external process per file can exceed the cost
  of hashing it, so cheap universal extractors should run in-process and
  external tools should run only for the types that need them.
- Timeouts, and how long a scan may hold exclusive access while extractors run.
- Optional sandboxing of decoders, for example without network access or under
  `bwrap`.
