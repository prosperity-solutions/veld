import { describe, expect, it } from "vitest";

import { formatRemaining } from "./useCaffeinate";

describe("formatRemaining", () => {
  it("drops the hours when there are none, and the minutes when they are zero", () => {
    // A bare "0m" reads as expired rather than as "nearly four hours", and
    // "4h 0m" is noise on the one label a user glances at.
    expect(formatRemaining(4 * 3600)).toBe("4h");
    expect(formatRemaining(4 * 3600 + 12 * 60)).toBe("4h 12m");
    expect(formatRemaining(12 * 60)).toBe("12m");
  });

  it("never renders a zero, so a finished countdown does not read as a live one", () => {
    // The daemon clamps `remaining_secs` at zero rather than going negative, so
    // this is the value the last poll before an expiry actually shows.
    expect(formatRemaining(0)).toBe("under a minute");
    expect(formatRemaining(59)).toBe("under a minute");
    expect(formatRemaining(60)).toBe("1m");
  });

  it("counts whole minutes down rather than rounding up to the next one", () => {
    // Rounding up would show "1h" for 59m30s and then jump to "59m" — a
    // countdown that appears to go backwards.
    expect(formatRemaining(3599)).toBe("59m");
    expect(formatRemaining(3600)).toBe("1h");
  });
});
