// Thinking updates must reach the user as messages. With thinking on, the
// model writes its between-tool narration as thinking "updates" rather than
// text blocks, so a turn can otherwise look like bare tool calls.

import { describe, expect, it } from "vitest";
import { activityToThreadMessages } from "../AcpRuntime";
import { type ActivityRow } from "../../../lib/acpTypes";

function row(id: string, kind: ActivityRow["kind"], text: string): ActivityRow {
  return { id, kind, text, at: "2026-09-07T02:19:46Z" };
}

function render(rows: ActivityRow[]): string {
  const messages = activityToThreadMessages(rows, false);
  return messages
    .flatMap((m) => (Array.isArray(m.content) ? m.content : []))
    .filter((p): p is { type: "text"; text: string } => (p as { type?: string }).type === "text")
    .map((p) => p.text)
    .join("\n@@\n");
}

describe("thinking updates", () => {
  it("renders update text instead of dropping it", () => {
    const out = render([row("t1", "thinking", "Both port forwards verified.")]);
    expect(out).toContain("Both port forwards verified.");
  });

  it("shows it as plain message text under an update tag", () => {
    const out = render([row("t1", "thinking", "Handing this to the implementer.")]);
    expect(out).toBe("*↳ update*\n\nHanding this to the implementer.");
  });

  it("keeps an update out of an adjacent assistant message", () => {
    const out = render([
      row("t1", "thinking", "I should check the ports."),
      row("m1", "message", "Both ports forward correctly."),
    ]);
    const parts = out.split("\n@@\n");
    expect(parts.length).toBeGreaterThan(1);
    const answer = parts.find((p) => p.includes("Both ports forward correctly."));
    expect(answer).toBeDefined();
    expect(answer).not.toContain("I should check the ports.");
    expect(answer).not.toContain("↳ update");
  });

  // Fragments exactly as a real update streamed on 2026-09-08. They split
  // mid-word ("met" + "adata") and carry the spaces between words at their
  // edges, so any per-fragment formatting shows up as stray spaces.
  it("joins streamed fragments verbatim, even when they split mid-word", () => {
    const fragments = [
      "Found that E",
      "UINHU26-139433",
      "'s printed date",
      " (Sept 1, ",
      "2026) diverges from the",
      " API/",
      "met",
      "adata tim",
      "estamp (Sept ",
      "2, 2",
      "026 ",
      "U",
      "TC).",
    ];
    const out = render(fragments.map((f, i) => row(`t${i}`, "thinking", f)));
    expect(out.match(/↳ update/g) ?? []).toHaveLength(1);
    expect(out).toContain(
      "Found that EUINHU26-139433's printed date (Sept 1, 2026) diverges from the API/metadata timestamp (Sept 2, 2026 UTC).",
    );
  });

  it("keeps a whitespace-only fragment that falls between words", () => {
    const out = render([row("t1", "thinking", "word"), row("t2", "thinking", " "), row("t3", "thinking", "next")]);
    expect(out).toContain("word next");
  });

  it("keeps a line break that spans fragments", () => {
    const out = render([row("t1", "thinking", "First line\nSec"), row("t2", "thinking", "ond line")]);
    expect(out).toContain("First line\nSecond line");
  });

  it("ignores an empty update rather than emitting a bare tag", () => {
    const out = render([row("t1", "thinking", "   ")]);
    expect(out).not.toContain("↳ update");
  });
});
