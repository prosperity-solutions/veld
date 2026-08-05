import { describe, expect, it } from "vitest";

import { AUTO, MASKS } from "./qrFixtures";
import {
  QR_QUIET_ZONE,
  QR_SCREEN_QUIET_ZONE,
  VELD_MARK,
  encodeQr,
  qrPath,
  qrRenderSize,
  qrViewBox,
  veldMarkBox,
} from "./qr";

/** A matrix as one string per row, the shape the fixtures are stored in. */
function rows(m: boolean[][]): string[] {
  return m.map((row) => row.map((v) => (v ? "1" : "0")).join(""));
}

function label(text: string): string {
  return text.length > 28 ? `${text.slice(0, 28)}… (${text.length}B)` : text;
}

describe("encodeQr", () => {
  // The point of this file: every module of every fixture, against an
  // implementation that shares no code with ours. A subtly wrong encoder still
  // renders a plausible square, so nothing weaker than a whole-matrix comparison
  // tells us a scanner could read it. Both bugs this caught during development —
  // transposed format information, and an alignment pattern dropped from version 7
  // up — produced completely normal-looking output.
  for (const f of AUTO) {
    it(`matches the reference for ${label(f.text)}`, () => {
      const qr = encodeQr(f.text);
      expect(qr).not.toBeNull();
      expect(qr!.version).toBe(f.version);
      expect(qr!.size).toBe(17 + 4 * f.version);
      expect(rows(qr!.modules)).toEqual(f.rows);
    });
  }

  for (const f of MASKS) {
    it(`matches the reference under mask ${f.mask}`, () => {
      // Masking, the format bits and the data walk, checked without the penalty
      // rules in the way: a mask mismatch and a codeword mismatch are entirely
      // different bugs that fail identically when only the auto path is tested.
      expect(rows(encodeQr(f.text, f.mask)!.modules)).toEqual(f.rows);
    });
  }

  it("selects the mask the penalty rules prefer", () => {
    // Implied by the AUTO comparisons, but asserted directly so a mask-selection
    // regression names itself. `AUTO`'s masks are the spec argmin, re-scored by an
    // independent implementation — see the fixture file's header.
    for (const f of AUTO) {
      expect(rows(encodeQr(f.text)!.modules), label(f.text)).toEqual(
        rows(encodeQr(f.text, f.mask)!.modules),
      );
    }
  });

  it("refuses a payload past version 10 rather than truncating it", () => {
    // The failure mode this guards is the ugly one: a code that encodes the first
    // 213 bytes of a URL scans fine and sends someone to the wrong place.
    expect(encodeQr("x".repeat(213))).not.toBeNull();
    expect(encodeQr("x".repeat(214))).toBeNull();
  });

  it("counts payload length in UTF-8 bytes, not characters", () => {
    // 106 characters, 212 bytes — the limit is on the encoded length, and a
    // character-counting version chooser would overflow the symbol it picked.
    expect(encodeQr("é".repeat(106))).not.toBeNull();
    expect(encodeQr("é".repeat(107))).toBeNull();
  });

  it("places the three finder patterns and the dark module", () => {
    const qr = encodeQr("https://veld.oss.life.li")!;
    for (const [cx, cy] of [
      [0, 0],
      [qr.size - 7, 0],
      [0, qr.size - 7],
    ]) {
      // Outer ring dark, the ring inside it light, 3×3 core dark.
      expect(qr.modules[cy][cx]).toBe(true);
      expect(qr.modules[cy + 1][cx + 1]).toBe(false);
      expect(qr.modules[cy + 3][cx + 3]).toBe(true);
    }
    expect(qr.modules[qr.size - 8][8]).toBe(true);
  });

  it("keeps the alignment pattern that sits on the timing column", () => {
    // Versions 7 and up put an alignment centre on column 6, where the timing
    // pattern already is. Skipping it there — "something is already written here" —
    // shifts every data module after it and was a real bug in this file.
    const qr = encodeQr("x".repeat(122))!;
    expect(qr.version).toBe(7);
    // Centre (6, 22): dark core, light ring, dark outer ring.
    expect(qr.modules[22][6]).toBe(true);
    expect(qr.modules[21][5]).toBe(false);
    expect(qr.modules[20][4]).toBe(true);
  });
});

describe("qrPath", () => {
  it("emits one horizontal run per group of dark modules", () => {
    const qr = encodeQr("https://veld.oss.life.li")!;
    const path = qrPath(qr);
    // A finder pattern's top edge is seven dark modules, so the path must contain a
    // seven-wide run rather than seven one-wide ones.
    expect(path).toContain("h7");
    // Every command is a closed rectangle; nothing else is emitted.
    expect(path.match(/M/g)!.length).toBe(path.match(/z/g)!.length);
  });

  it("offsets by the quiet zone and sizes the viewBox to match", () => {
    const qr = encodeQr("https://veld.oss.life.li")!;
    // The top-left finder starts at module (0,0), which the path must place at the
    // quiet-zone offset — a QR flush against the edge of its SVG is unscannable.
    expect(qrPath(qr).startsWith(`M${QR_QUIET_ZONE} ${QR_QUIET_ZONE}h7`)).toBe(true);
    expect(qrViewBox(qr)).toBe(qr.size + 2 * QR_QUIET_ZONE);
  });

  it("draws nothing outside the matrix", () => {
    const qr = encodeQr("https://veld.oss.life.li")!;
    const max = qrViewBox(qr);
    for (const [, x, y, w] of qrPath(qr).matchAll(/M(\d+) (\d+)h(\d+)/g)) {
      expect(Number(x) + Number(w)).toBeLessThanOrEqual(max - QR_QUIET_ZONE);
      expect(Number(y)).toBeLessThan(max - QR_QUIET_ZONE);
    }
  });
});

describe("quiet zones", () => {
  it("offsets the path and the viewBox by the same amount", () => {
    // The screen renders 2 and the copied PNG renders 4. If the path and the viewBox
    // ever disagreed, the code would be drawn off-centre in its white field — which is
    // a scannable-looking image with a clipped quiet zone on one side.
    const qr = encodeQr("https://veld.oss.life.li")!;
    for (const quiet of [QR_SCREEN_QUIET_ZONE, QR_QUIET_ZONE]) {
      expect(qrViewBox(qr, quiet)).toBe(qr.size + 2 * quiet);
      expect(qrPath(qr, quiet).startsWith(`M${quiet} ${quiet}h7`)).toBe(true);
      // Nothing may be drawn into the margin on the far side either.
      for (const [, x, y, w] of qrPath(qr, quiet).matchAll(/M(\d+) (\d+)h(\d+)/g)) {
        expect(Number(x) + Number(w)).toBeLessThanOrEqual(qr.size + quiet);
        expect(Number(y)).toBeLessThan(qr.size + quiet);
      }
    }
  });

  it("keeps the full four modules for the copied image", () => {
    // A pasted PNG has no white card around it: the chat's own background starts at
    // the image edge, so the spec's minimum has to be inside the picture.
    expect(QR_QUIET_ZONE).toBe(4);
    expect(QR_SCREEN_QUIET_ZONE).toBeLessThan(QR_QUIET_ZONE);
  });
});

describe("veldMarkBox", () => {
  it("centres the mark inside whichever quiet zone it is given", () => {
    const qr = encodeQr("https://veld.oss.life.li")!;
    for (const quiet of [QR_SCREEN_QUIET_ZONE, QR_QUIET_ZONE]) {
      const { side, origin } = veldMarkBox(qr.size, quiet);
      expect(origin + side / 2).toBeCloseTo(qrViewBox(qr, quiet) / 2);
    }
  });

  it("sizes the mark identically for every quiet zone", () => {
    // The property the two renderers depend on. The on-screen SVG draws a 2-module quiet
    // zone and the copied PNG draws 4; when `side` was derived from the padded box, the
    // two disagreed for 7 of the 10 versions — while the comment beside them claimed
    // sharing the constant guaranteed they matched. A review angle caught the claim; this
    // pins the fix.
    for (const bytes of [10, 40, 80, 120, 213]) {
      const qr = encodeQr("x".repeat(bytes))!;
      expect(veldMarkBox(qr.size, QR_SCREEN_QUIET_ZONE).side).toBe(
        veldMarkBox(qr.size, QR_QUIET_ZONE).side,
      );
    }
  });

  it("stays inside the error-correction budget at every version", () => {
    // The load-bearing assertion in this file after the matrices themselves: the mark
    // is damage the decoder repairs out of level M's ~15%, and past roughly 7% of the
    // area codes start failing *intermittently* on cheap scanners. A future tweak to
    // `fraction` that looks harmless fails here instead of in someone's hand.
    for (const bytes of [10, 100, 213]) {
      const qr = encodeQr("x".repeat(bytes))!;
      const { side } = veldMarkBox(qr.size, QR_SCREEN_QUIET_ZONE);
      const covered = (side * side) / (qr.size * qr.size);
      expect(covered).toBeLessThan(0.07);
    }
  });

  it("never shrinks below a legible side", () => {
    // A version-1 symbol is 21 modules; 18% of it would be under four, at which point
    // the mark is a smudge rather than a logo.
    const qr = encodeQr("hi")!;
    expect(veldMarkBox(qr.size, QR_SCREEN_QUIET_ZONE).side).toBeGreaterThanOrEqual(
      VELD_MARK.minSide,
    );
  });
});

describe("qrRenderSize", () => {
  it("is always an integer number of pixels per module", () => {
    // The reason this function exists: `viewBox` units are modules, so a fractional
    // scale puts module edges on fractional pixels and the browser anti-aliases every
    // one of them into grey — which is the contrast a scanner is looking for.
    for (const box of [21, 25, 29, 33, 37, 41, 45, 49, 53, 57, 65]) {
      for (const target of [64, 90, 108, 148, 200]) {
        expect(qrRenderSize(box, target) % box).toBe(0);
      }
    }
  });

  it("takes the largest scale that fits the target", () => {
    expect(qrRenderSize(29, 108)).toBe(87); // 3 px per module, exactly the target
    expect(qrRenderSize(21, 108)).toBe(105); // 5 px per module
  });

  it("keeps three pixels per module even when that overshoots the target", () => {
    // The floor beats the target, deliberately: at two device pixels a module a
    // version-2 symbol with the centre mark failed to decode (see MIN_MODULE_PX), and a
    // code that is larger than asked for is a cosmetic complaint where one that does not
    // scan is the feature not working.
    expect(qrRenderSize(45, 108)).toBe(135);
    expect(qrRenderSize(65, 108)).toBe(195);
    expect(qrRenderSize(65, 64)).toBe(195);
  });

  it("never renders below the measured decodable floor", () => {
    for (const box of [21, 25, 29, 33, 37, 41, 45, 49, 53, 57, 65]) {
      for (const target of [32, 64, 90, 108, 200]) {
        expect(qrRenderSize(box, target) / box).toBeGreaterThanOrEqual(3);
      }
    }
  });
});
