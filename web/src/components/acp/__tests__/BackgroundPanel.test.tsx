// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { BackgroundPanel } from "../BackgroundPanel";
import { clockTime } from "../../../lib/background";
import type { BackgroundSummary } from "../../../lib/types";

const now = new Date().toISOString();
const summary: BackgroundSummary = {
  live: 2,
  lost_since: now,
  items: [
    { kind: "subagent", id: "a1", label: "review", started_at: now },
    { kind: "monitor", id: "m1", label: "adam run", started_at: now },
    {
      kind: "shell",
      id: "s1",
      label: "make test",
      started_at: now,
      ended: { reason: "lost", cause: "respawn", at: now },
    },
  ],
};

describe("BackgroundPanel", () => {
  it("lists live and ended items with a header count", () => {
    render(<BackgroundPanel sessionId="s-panel" summary={summary} turnActive={false} />);
    expect(screen.getByTestId("background-panel").textContent).toContain("Background: 2 live, 1 lost");
    expect(screen.getByText(/Monitor · adam run · live/)).toBeTruthy();
    expect(screen.getByText(/Shell · make test · lost/)).toBeTruthy();
  });

  it("says what an open turn is waiting on", () => {
    render(<BackgroundPanel sessionId="s-wait" summary={summary} turnActive />);
    expect(screen.getByText("The current turn is waiting on 1 sub-agent.")).toBeTruthy();
  });

  it("acknowledges a loss when shown, stamped with the server's loss time", () => {
    window.localStorage.removeItem("aoe.backgroundAck.s-ack");
    render(<BackgroundPanel sessionId="s-ack" summary={summary} turnActive={false} />);
    // Not the browser clock: a stamp of `lost_since` survives clock skew
    // between the browser and the server that computed it.
    expect(window.localStorage.getItem("aoe.backgroundAck.s-ack")).toBe(summary.lost_since);
  });

  it("renders nothing without items", () => {
    const { container } = render(<BackgroundPanel sessionId="s-empty" summary={undefined} turnActive={false} />);
    expect(container.firstChild).toBeNull();
  });

  it("shows each row's armed clock time", () => {
    render(<BackgroundPanel sessionId="s-clock" summary={summary} turnActive={false} />);
    const armed = new RegExp(`armed ${clockTime(now)}`);
    expect(screen.getAllByText(armed).length).toBe(summary.items.length);
  });

  it("opens the Background agents pane for a sub-agent row instead of expanding it", () => {
    const onOpenAgentsPane = vi.fn();
    render(
      <BackgroundPanel
        sessionId="s-subagent"
        summary={summary}
        turnActive={false}
        onOpenAgentsPane={onOpenAgentsPane}
      />,
    );
    fireEvent.click(screen.getByText(/Sub-agent · review · live/));
    expect(onOpenAgentsPane).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("review", { selector: "pre" })).toBeNull();
  });

  it("still expands a monitor row's label on click", () => {
    render(<BackgroundPanel sessionId="s-monitor" summary={summary} turnActive={false} />);
    fireEvent.click(screen.getByText(/Monitor · adam run · live/));
    expect(screen.getByText("adam run", { selector: "pre" })).toBeTruthy();
  });

  it("caps the item list's height so only it scrolls, not the header", () => {
    render(<BackgroundPanel sessionId="s-scroll" summary={summary} turnActive={false} />);
    const list = screen.getByTestId("background-panel").querySelector("ul");
    expect(list?.className).toContain("max-h-[20vh]");
    expect(list?.className).toContain("overflow-y-auto");
  });
});
