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

  it("joins a run of streamed thought chunks into one block", () => {
    const out = render([row("t1", "thinking", "Checking the"), row("t2", "thinking", "port forwards.")]);
    expect(out.match(/💭/g) ?? []).toHaveLength(1);
    expect(out).toContain("Checking the");
    expect(out).toContain("port forwards.");
  });

  it("ignores an empty summary rather than emitting an empty quote", () => {
    const out = render([row("t1", "thinking", "   ")]);
    expect(out).not.toContain("💭");
  });
});
