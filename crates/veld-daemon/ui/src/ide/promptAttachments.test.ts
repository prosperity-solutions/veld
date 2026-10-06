import { describe, expect, it } from "vitest";

import {
  addAttachments,
  extensionOf,
  formatSize,
  isTemporaryPath,
  isThumbnailable,
  MAX_ATTACHMENTS,
  MAX_UPLOAD_BYTES,
  type PromptAttachment,
  STRIP_FADE_LABEL,
  STRIP_FADE_MIN,
  snapshotIncoming,
  stripFade,
} from "./promptAttachments";

/** A `File` of `size` bytes without allocating them where it matters. */
const file = (name: string, size = 3, type = "", lastModified = 1) =>
  new File([new Uint8Array(size)], name, { type, lastModified });

/** Deterministic ids, `a0`, `a1`, … */
const ids = () => {
  let n = 0;
  return () => `a${n++}`;
};

describe("extensionOf", () => {
  it("lowercases the last extension", () => {
    expect(extensionOf("Shot.PNG")).toBe("png");
    expect(extensionOf("archive.tar.gz")).toBe("gz");
  });

  it("treats a dotfile's name and a bare name as having none", () => {
    expect(extensionOf(".env")).toBe("");
    expect(extensionOf("Makefile")).toBe("");
  });
});

describe("isThumbnailable", () => {
  it("trusts a MIME type when there is one", () => {
    expect(isThumbnailable("x.bin", "image/png")).toBe(true);
    // HEIC is an image Chromium does not draw: a glyph, not a broken box.
    expect(isThumbnailable("x.heic", "image/heic")).toBe(false);
    expect(isThumbnailable("x.png", "application/json")).toBe(false);
  });

  it("falls back to the extension when the type is empty", () => {
    expect(isThumbnailable("shot.webp", "")).toBe(true);
    expect(isThumbnailable("data.json", "")).toBe(false);
  });
});

describe("addAttachments", () => {
  it("keeps the desktop path and marks images", () => {
    const { next, refused } = addAttachments(
      [],
      [
        { file: file("shot.png", 3, "image/png"), path: "/Users/me/shot.png" },
        { file: file("data.json"), path: "/Users/me/data.json" },
      ],
      ids(),
    );
    expect(refused).toEqual([]);
    expect(next.map((a) => [a.id, a.name, a.path, a.image])).toEqual([
      ["a0", "shot.png", "/Users/me/shot.png", true],
      ["a1", "data.json", "/Users/me/data.json", false],
    ]);
  });

  it("skips a file already attached, by path or by identity, silently", () => {
    const makeId = ids();
    const first = addAttachments(
      [],
      [
        { file: file("a.png"), path: "/a.png" },
        { file: file("b.png", 5, "", 7), path: null },
      ],
      makeId,
    ).next;
    const { next, refused } = addAttachments(
      first,
      [
        { file: file("renamed-but-same-path.png"), path: "/a.png" },
        { file: file("b.png", 5, "", 7), path: null },
        // Same name, different bytes: a different file.
        { file: file("b.png", 6, "", 7), path: null },
      ],
      makeId,
    );
    expect(refused).toEqual([]);
    expect(next).toHaveLength(3);
  });

  it("refuses what a browser tab could never upload, and says why", () => {
    const { next, refused } = addAttachments(
      [],
      [
        { file: file("folder", 0), path: null },
        { file: file("huge.mov", MAX_UPLOAD_BYTES + 1), path: null },
      ],
      ids(),
    );
    expect(next).toEqual([]);
    expect(refused).toEqual([
      "folder is empty or a folder — a browser tab can only attach files with content",
      "huge.mov is larger than 32 MB",
    ]);
  });

  it("does not hold a desktop path to the upload limits", () => {
    // A path is pasted, never uploaded — and a desktop folder has one.
    const { next, refused } = addAttachments(
      [],
      [
        { file: file("folder", 0), path: "/Users/me/folder" },
        { file: file("huge.mov", MAX_UPLOAD_BYTES + 1), path: "/Users/me/huge.mov" },
      ],
      ids(),
    );
    expect(refused).toEqual([]);
    expect(next).toHaveLength(2);
  });

  it("stops at the cap and counts what it left out", () => {
    const current: PromptAttachment[] = addAttachments(
      [],
      Array.from({ length: MAX_ATTACHMENTS - 1 }, (_, i) => ({
        file: file(`f${i}`),
        path: `/f${i}`,
      })),
      ids(),
    ).next;
    const { next, refused } = addAttachments(
      current,
      [
        { file: file("x"), path: "/x" },
        { file: file("y"), path: "/y" },
        { file: file("z"), path: "/z" },
      ],
      ids(),
    );
    expect(next).toHaveLength(MAX_ATTACHMENTS);
    expect(next.at(-1)?.path).toBe("/x");
    expect(refused).toEqual([`2 files were not attached — ${MAX_ATTACHMENTS} is the limit`]);
  });

  it("does not change the list it was handed", () => {
    const current = addAttachments([], [{ file: file("a"), path: "/a" }], ids()).next;
    addAttachments(current, [{ file: file("b"), path: "/b" }], ids());
    expect(current).toHaveLength(1);
  });
});

describe("isTemporaryPath", () => {
  it("knows the macOS screenshot thumbnail's folder and the usual temp roots", () => {
    expect(
      isTemporaryPath("/var/folders/x1/q9/T/TemporaryItems/NSIRD_screencaptureui_O0/Bildschirmfoto 1.png"),
    ).toBe(true);
    expect(isTemporaryPath("/private/var/folders/x1/q9/T/a.png")).toBe(true);
    expect(isTemporaryPath("/tmp/a.json")).toBe(true);
    expect(isTemporaryPath("C:\\Users\\me\\AppData\\Local\\Temp\\a.png")).toBe(true);
  });

  it("leaves ordinary paths alone", () => {
    expect(isTemporaryPath("/Users/me/Desktop/shot.png")).toBe(false);
    expect(isTemporaryPath("/Users/me/tmp/a.json")).toBe(false);
    expect(isTemporaryPath("C:\\Users\\me\\a.png")).toBe(false);
  });
});

describe("formatSize", () => {
  it("rounds to a unit a person reads at a glance", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(1023)).toBe("1023 B");
    expect(formatSize(1024)).toBe("1 KB");
    expect(formatSize(250_000)).toBe("244 KB");
    expect(formatSize(1.5 * 1024 * 1024)).toBe("1.5 MB");
    expect(formatSize(20 * 1024 * 1024)).toBe("20 MB");
    // Just under a boundary reads as the unit it rounds to.
    expect(formatSize(1_048_500)).toBe("1.0 MB");
    expect(formatSize(9.96 * 1024 * 1024)).toBe("10 MB");
  });
});

describe("snapshotIncoming", () => {
  it("copies a temporary file's bytes and drops its path, remembering where it came from", async () => {
    const temp = "/var/folders/x/T/TemporaryItems/NSIRD_screencaptureui_A/shot.png";
    const original = file("shot.png", 5, "image/png", 7);
    const out = await snapshotIncoming({ file: original, path: temp });
    expect(out.path).toBeNull();
    expect(out.copiedFrom).toBe(temp);
    expect(out.file).not.toBe(original);
    expect([out.file.name, out.file.size, out.file.type, out.file.lastModified]).toEqual([
      "shot.png",
      5,
      "image/png",
      7,
    ]);
  });

  it("passes everything else through untouched", async () => {
    const plain = { file: file("a.json"), path: "/Users/me/a.json" };
    expect(await snapshotIncoming(plain)).toBe(plain);
    const browser = { file: file("a.json"), path: null };
    expect(await snapshotIncoming(browser)).toBe(browser);
    // Nothing to copy, or too much: the path is the better thing to hand over.
    const empty = { file: file("e", 0), path: "/tmp/e" };
    expect(await snapshotIncoming(empty)).toBe(empty);
  });

  it("keeps the path when the bytes are already gone", async () => {
    const gone = new File([], "x.png");
    Object.defineProperty(gone, "size", { value: 3 });
    gone.arrayBuffer = () => Promise.reject(new Error("NotFoundError"));
    const incoming = { file: gone, path: "/tmp/x.png" };
    expect(await snapshotIncoming(incoming)).toBe(incoming);
  });
});

describe("addAttachments with copies", () => {
  it("treats a second copy of the same temporary file as a duplicate", () => {
    const first = { file: file("s.png", 3, "", 1), path: null, copiedFrom: "/tmp/s.png" };
    const again = { file: file("s.png", 3, "", 2), path: null, copiedFrom: "/tmp/s.png" };
    const { next } = addAttachments([], [first, again], ids());
    expect(next.map((a) => [a.file.name, a.path, a.copiedFrom])).toEqual([["s.png", null, "/tmp/s.png"]]);
  });
});

describe("stripFade", () => {
  // Eleven 112px tiles, 8px apart, after 10px of padding: the strip in the dialog.
  const tiles = Array.from({ length: 11 }, (_, i) => ({ left: 10 + i * 120, right: 122 + i * 120 }));
  const width = 500;
  const end = tiles[10].right + 10 - width;

  it("is nothing when every tile fits", () => {
    expect(stripFade({ scrollLeft: 0, clientWidth: 2000 }, tiles)).toEqual({ width: 0, more: 0 });
    expect(stripFade({ scrollLeft: 0, clientWidth: 500 }, [])).toEqual({ width: 0, more: 0 });
  });

  it("at rest covers the first tile that does not fit, and counts it among the rest", () => {
    // Tile 5 starts at 490 and is cut at 500: 10px of it shows, so the fade is
    // its label's width, reaching back over tile 4's end.
    expect(stripFade({ scrollLeft: 0, clientWidth: width }, tiles)).toEqual({ width: STRIP_FADE_LABEL, more: 7 });
    // However much of the cut tile shows, all of it is under the fade: tile 4
    // starts at 490 and shows 110px of itself in a 600px strip.
    for (const clientWidth of [500, 540, 580, 600]) {
      const { width } = stripFade({ scrollLeft: 0, clientWidth }, tiles);
      expect(clientWidth - width).toBeLessThanOrEqual(490);
    }
    // A wider cut than the label needs is covered from the tile's own left edge.
    const wide = [{ left: 0, right: 300 }, { left: 310, right: 610 }];
    expect(stripFade({ scrollLeft: 0, clientWidth: 500 }, wide)).toEqual({ width: 190, more: 1 });
    // Tile 3 shows 30px of itself: the fade is the label's width, and it is one of eight.
    expect(stripFade({ scrollLeft: 0, clientWidth: 400 }, tiles)).toEqual({ width: STRIP_FADE_LABEL, more: 8 });
  });

  it("shrinks as it scrolls, to a sliver that stays while a tile is still out of view", () => {
    expect(stripFade({ scrollLeft: 40, clientWidth: width }, tiles).width).toBe(STRIP_FADE_LABEL - 40);
    expect(stripFade({ scrollLeft: 300, clientWidth: width }, tiles).width).toBe(STRIP_FADE_MIN);
    // One pixel short of the end, the last tile is still cut: the sliver stays.
    expect(stripFade({ scrollLeft: end - 11, clientWidth: width }, tiles)).toEqual({ width: STRIP_FADE_MIN, more: 1 });
  });

  it("is gone once the last tile is fully in view", () => {
    expect(stripFade({ scrollLeft: end - 10, clientWidth: width }, tiles)).toEqual({ width: 0, more: 0 });
    expect(stripFade({ scrollLeft: end, clientWidth: width }, tiles)).toEqual({ width: 0, more: 0 });
  });

  it("is never wider than the strip", () => {
    expect(stripFade({ scrollLeft: 0, clientWidth: 90 }, tiles).width).toBe(90);
  });
});
