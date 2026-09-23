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

// Agents in `[acp] reasoning_agents` (grok) stream raw reasoning on the
// thought channel, not narration. It becomes one `reasoning` part per run,
// which StructuredView renders collapsed, and never lands in message text.
describe("raw reasoning (thought_display = reasoning)", () => {
  function parts(rows: ActivityRow[]) {
    return activityToThreadMessages(rows, false, false, true, undefined, "reasoning").flatMap((m) =>
      Array.isArray(m.content) ? (m.content as { type: string; text?: string }[]) : [],
    );
  }

  it("keeps a run of thought fragments as one reasoning part, lone spaces included", () => {
    const out = parts([
      row("t1", "thinking", "Now read-only survey of all"),
      row("t2", "thinking", " "),
      row("t3", "thinking", "8 nodes."),
    ]);
    const reasoning = out.filter((p) => p.type === "reasoning");
    expect(reasoning).toEqual([{ type: "reasoning", text: "Now read-only survey of all 8 nodes." }]);
    expect(out.some((p) => p.type === "text" && (p.text ?? "").includes("survey"))).toBe(false);
  });

  it("puts the reply after it in its own text part, without the update tag", () => {
    const out = parts([row("t1", "thinking", "I should verify the command."), row("m1", "message", "SSH works.")]);
    expect(out.map((p) => p.type)).toEqual(["reasoning", "text"]);
    expect(out[1].text).toBe("SSH works.");
    expect(out[1].text).not.toContain("↳ update");
  });

  it("starts a new reasoning part after the reply", () => {
    const out = parts([row("t1", "thinking", "first"), row("m1", "message", "reply"), row("t2", "thinking", "second")]);
    expect(out.map((p) => p.type)).toEqual(["reasoning", "text", "reasoning"]);
  });
});
