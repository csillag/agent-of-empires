// Reasoning-summary rows must reach the user (#fable-narration): recent
// models route what reads as narration into the thought channel instead of
// emitting a text block, so a turn can arrive as bare tool calls with no
// prose. The rows are rendered, and rendered as something visibly distinct
// from a message the agent addressed to the user.

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

describe("reasoning-summary rows", () => {
  it("renders thought text instead of dropping it", () => {
    const out = render([row("t1", "thinking", "Both port forwards verified.")]);
    expect(out).toContain("Both port forwards verified.");
  });

  it("marks it as thinking and quotes it, so it cannot read as an answer", () => {
    const out = render([row("t1", "thinking", "Handing this to the implementer.")]);
    expect(out).toContain("💭");
    expect(out).toContain("> Handing this to the implementer.");
  });

  it("keeps a summary out of an adjacent assistant message", () => {
    const out = render([
      row("t1", "thinking", "I should check the ports."),
      row("m1", "message", "Both ports forward correctly."),
    ]);
    // The two must not end up concatenated into one indistinguishable blob.
    const parts = out.split("\n@@\n");
    expect(parts.length).toBeGreaterThan(1);
    const answer = parts.find((p) => p.includes("Both ports forward correctly."));
    expect(answer).toBeDefined();
    expect(answer).not.toContain("I should check the ports.");
  });

  // Fragments exactly as a real summary streamed on 2026-09-08. They split
  // mid-word ("met" + "adata") and carry the spaces between words at their
  // edges. The first version of this test split only at word boundaries,
  // the one case where joining fragments with line breaks looks right; in
  // the dashboard it rendered "API/ met adata tim estamp (Sept 2, 2 026 U TC)".
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
    expect(out.match(/💭/g) ?? []).toHaveLength(1);
    expect(out).toContain(
      "> Found that EUINHU26-139433's printed date (Sept 1, 2026) diverges from the API/metadata timestamp (Sept 2, 2026 UTC).",
    );
  });

  it("keeps a whitespace-only fragment that falls between words", () => {
    const out = render([row("t1", "thinking", "word"), row("t2", "thinking", " "), row("t3", "thinking", "next")]);
    expect(out).toContain("> word next");
  });

  it("quotes every line of a summary whose line break spans fragments", () => {
    const out = render([row("t1", "thinking", "First line\nSec"), row("t2", "thinking", "ond line")]);
    expect(out).toContain("> First line\n> Second line");
  });

  it("ignores an empty summary rather than emitting an empty quote", () => {
    const out = render([row("t1", "thinking", "   ")]);
    expect(out).not.toContain("💭");
  });
});
