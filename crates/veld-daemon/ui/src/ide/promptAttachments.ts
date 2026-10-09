/**
 * Files attached to the New worktree dialog's prompt.
 *
 * **They end as paths, the same as a file dropped on a terminal pane** — see
 * `panes/terminalHost.ts`'s `attachFileInput`. A pty carries bytes, so "give the
 * agent this screenshot" means "paste the path of an image file", and Claude Code
 * turns an image path into `[Image #1]`. The dialog only collects the files; the
 * pane that opens in the new checkout hands them over with the prompt — as
 * `@"path"` mentions in the launch argument when the agent takes one, typed in
 * after the prompt otherwise (`panes/promptDelivery.ts`, `deliverQueuedPrompt`).
 *
 * Where the path comes from differs by shell, and that difference is the reason
 * an attachment carries its `File` and not only a path:
 *
 * - **Desktop** — Electron resolves the real path at drop time, so the tile's
 *   tooltip shows it and nothing is copied. **Unless it is a temporary file** — see
 *   [`isTemporaryPath`]: those are read at drop time and handled as a browser
 *   tab's are.
 * - **Browser tab** — the File API withholds the path. The bytes are kept in
 *   memory and uploaded just before the agent's terminal starts, under an upload
 *   reservation (`pty.rs`'s `reserve_uploads`): the daemon's paste endpoint
 *   otherwise writes only for a live session, and there is none yet.
 *
 * **Not part of the prompt draft.** `ide/promptDraft.ts` keeps the text across a
 * close, but a browser `File` cannot be put in storage, and keeping the desktop
 * half only would make an attachment's survival depend on which shell you are in.
 */

/** One file waiting to go with the prompt. */
export interface PromptAttachment {
  /** Stable React key; unique within one dialog. */
  id: string;
  /** The file's own name, extension included — what the row leads with. */
  name: string;
  /**
   * The absolute path to paste, where there is one worth pasting — a desktop
   * file in an ordinary place. `null` means the bytes are uploaded instead.
   */
  path: string | null;
  /**
   * The temporary file the bytes were read from at drop time, for a desktop
   * file whose own path would be gone before the agent read it. `null`
   * otherwise. Only ever set alongside a `null` `path`.
   */
  copiedFrom: string | null;
  /** The bytes, for the upload a path-less attachment needs. */
  file: File;
  /** Whether the row can show the file itself as its thumbnail. */
  image: boolean;
}

/**
 * Most files one prompt may carry.
 *
 * The same bound a terminal drop has (`MAX_DROP_FILES` there) and for the same
 * reason: in a browser tab each one is an upload the daemon buffers whole, and a
 * drop is the gesture that hands over a folder's worth by accident.
 */
export const MAX_ATTACHMENTS = 20;

/**
 * Largest file a browser tab can attach — the daemon's `MAX_PASTE_BYTES`.
 *
 * Checked when the file is added rather than left to the upload, because the
 * upload happens after the dialog has closed: a refusal then arrives as a toast
 * about a prompt the user has stopped looking at, with the agent already started
 * on the rest. Only path-less files are held to it; a desktop path is never
 * uploaded.
 */
export const MAX_UPLOAD_BYTES = 32 * 1024 * 1024;

/**
 * Image types Chromium can draw in an `<img>`, which is what a thumbnail is.
 *
 * By extension as well as MIME type, because a drag from some apps arrives with
 * an empty `type`. HEIC and TIFF are images Chromium does not render, so they get
 * the generic file icon rather than a broken thumbnail.
 */
const THUMBNAIL_EXTENSIONS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "avif", "bmp", "ico"]);
const THUMBNAIL_TYPES = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/svg+xml",
  "image/avif",
  "image/bmp",
  "image/x-icon",
  "image/vnd.microsoft.icon",
]);

/** The extension of a file name, lowercased, or `""` for none. */
export function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  // A leading dot is a dotfile's name, not an extension (`.env`).
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/** Whether a file can be shown as its own thumbnail. */
export function isThumbnailable(name: string, type: string): boolean {
  const mime = type.toLowerCase().split(";")[0].trim();
  if (mime !== "") return THUMBNAIL_TYPES.has(mime);
  return THUMBNAIL_EXTENSIONS.has(extensionOf(name));
}

/**
 * Whether a path is somewhere the OS cleans up on its own schedule.
 *
 * **Why this matters: the agent reads the file seconds later, not now.** The
 * path is pasted once the new checkout's agent has started, and a temporary file
 * can be gone by then. The case that found this is the macOS screenshot
 * thumbnail: dragging it hands over a file in
 * `$TMPDIR/TemporaryItems/NSIRD_screencaptureui_…/`, which macOS removes once
 * the drag is done — so the agent was handed a path to nothing, and Claude Code,
 * failing to read it, typed it in as text instead of attaching the image.
 *
 * By location rather than by app, because the hazard is the folder: macOS's
 * per-user `$TMPDIR` (`/var/folders/…`, also reached through `/private`), the
 * shared `/tmp`, and Windows' `AppData\Local\Temp`.
 */
export function isTemporaryPath(path: string): boolean {
  return /^(\/private)?\/(var\/folders|tmp)\//.test(path) || /[\\/]AppData[\\/]Local[\\/]Temp[\\/]/i.test(path);
}

/** A file size as a row shows it: `0 B`, `812 B`, `41 KB`, `4.1 MB`. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  // Each unit is decided on the number as shown, not the raw one, so a size
  // just under a boundary reads `1.0 MB` rather than `1024 KB`, and `10 MB`
  // rather than `10.0 MB`.
  const kb = Math.round(bytes / 1024);
  if (kb < 1024) return `${kb} KB`;
  const mb = bytes / 1024 / 1024;
  // One decimal while it says something (`4.1 MB`), none once it does not.
  return Number(mb.toFixed(1)) < 10 ? `${mb.toFixed(1)} MB` : `${Math.round(mb)} MB`;
}

/** What [`addAttachments`] is handed per file: the file, and its path if known. */
export interface IncomingFile {
  file: File;
  path: string | null;
  /** See [`PromptAttachment.copiedFrom`]; set by [`snapshotIncoming`]. */
  copiedFrom?: string | null;
}

/**
 * Read a temporary file's bytes now, while it still exists, and hand it on as a
 * copy to upload rather than a path to paste.
 *
 * Call it **in the drop's own turn**: the read starts when this is called, and
 * a temporary file is exactly the one that may not outlast the gesture.
 *
 * Everything else passes through untouched — an ordinary path, a browser tab's
 * path-less file. A temporary file the upload would refuse anyway (empty, or
 * past [`MAX_UPLOAD_BYTES`]) keeps its path, which may still be there when the
 * agent looks; so does one whose read fails.
 */
export async function snapshotIncoming(incoming: IncomingFile): Promise<IncomingFile> {
  const { file, path } = incoming;
  if (path === null || !isTemporaryPath(path)) return incoming;
  if (file.size === 0 || file.size > MAX_UPLOAD_BYTES) return incoming;
  try {
    const bytes = await file.arrayBuffer();
    const copy = new File([bytes], file.name, { type: file.type, lastModified: file.lastModified });
    return { file: copy, path: null, copiedFrom: path };
  } catch {
    return incoming;
  }
}

/** The outcome of adding files: the new list, and a sentence per file refused. */
export interface AddResult {
  next: PromptAttachment[];
  refused: string[];
}

/**
 * Add files to the list, refusing the ones that could never be delivered.
 *
 * **Refused here, said here.** Every refusal below is a delivery that would fail
 * after the dialog closed, where the only voice left is a toast about a prompt
 * already on its way. Saying it while the row would have been on screen is the
 * point of checking early.
 *
 * - **Already attached** — the same path, or for a path-less file the same
 *   name, size and modification time. Skipped silently: dropping the same
 *   screenshot twice is a slip, not something to be told about.
 * - **Empty, path-less** — the daemon refuses an empty upload, and it is also
 *   what a dropped *folder* looks like in a browser tab (no readable bytes). On
 *   the desktop a folder has a path and is kept: an agent can list it.
 * - **Too large, path-less** — past [`MAX_UPLOAD_BYTES`].
 * - **Past the cap** — [`MAX_ATTACHMENTS`], counted against what is already
 *   attached.
 *
 * `makeId` is injected so the ids are deterministic in a test.
 */
export function addAttachments(
  current: readonly PromptAttachment[],
  incoming: readonly IncomingFile[],
  makeId: () => string,
): AddResult {
  const next = [...current];
  const refused: string[] = [];
  let overCap = 0;
  for (const { file, path, copiedFrom = null } of incoming) {
    const duplicate = next.some((a) =>
      path !== null
        ? a.path === path
        : copiedFrom !== null
          ? a.copiedFrom === copiedFrom
          : a.path === null &&
          a.copiedFrom === null &&
          a.name === file.name &&
          a.file.size === file.size &&
          a.file.lastModified === file.lastModified,
    );
    if (duplicate) continue;
    if (path === null && file.size === 0) {
      refused.push(`${file.name} is empty or a folder — a browser tab can only attach files with content`);
      continue;
    }
    if (path === null && file.size > MAX_UPLOAD_BYTES) {
      refused.push(`${file.name} is larger than 32 MB`);
      continue;
    }
    if (next.length >= MAX_ATTACHMENTS) {
      overCap += 1;
      continue;
    }
    next.push({
      id: makeId(),
      name: file.name,
      path,
      copiedFrom,
      file,
      image: isThumbnailable(file.name, file.type),
    });
  }
  if (overCap > 0) {
    refused.push(
      overCap === 1
        ? `1 file was not attached — ${MAX_ATTACHMENTS} is the limit`
        : `${overCap} files were not attached — ${MAX_ATTACHMENTS} is the limit`,
    );
  }
  return { next, refused };
}

/** Where one tile sits in the strip, in scroll-content pixels. */
export interface TileSpan {
  left: number;
  right: number;
}

/** The narrowest the strip's fade gets while anything is still out of view. */
export const STRIP_FADE_MIN = 28;
/** The fade's width at rest, at least — room for "11 more files". */
export const STRIP_FADE_LABEL = 120;

/**
 * The fade over the right end of the one-row attachment strip, at one scroll
 * position: how wide it is, and how many tiles it stands for.
 *
 * At rest it covers the first tile that does not fit, from that tile's left
 * edge, and is never narrower than the room its "N more files" label needs.
 * Scrolling shrinks it by the distance scrolled, revealing what it covered,
 * down to a sliver that stays for as long as **any tile is not fully in view**
 * — the only thing it promises. Once the last tile's right edge is in, it is
 * gone, so at the end of the strip nothing is covered.
 *
 * Shrinking with the scroll offset, rather than re-covering whichever tile is
 * cut at each position, is deliberate: the cut tile changes every 120px, and a
 * fade that snapped back to full width each time would pulse under the finger.
 */
export function stripFade(
  view: { scrollLeft: number; clientWidth: number },
  tiles: readonly TileSpan[],
): { width: number; more: number } {
  // Half a pixel of slack on the far side: zoom and fractional layout put edges
  // between pixels, and a tile that overhangs by less is in view.
  const edge = view.scrollLeft + view.clientWidth + 0.5;
  const more = tiles.filter((t) => t.right > edge).length;
  if (more === 0) return { width: 0, more: 0 };
  const cut = tiles.find((t) => t.right > view.clientWidth + 0.5);
  const cover = cut === undefined ? 0 : view.clientWidth - cut.left;
  const atRest = Math.min(Math.max(cover, STRIP_FADE_LABEL), view.clientWidth);
  return { width: Math.max(STRIP_FADE_MIN, atRest - view.scrollLeft), more };
}
