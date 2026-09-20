// @vitest-environment jsdom
//
// The keep-alive host owns which view is live; StructuredView only forwards
// that decision to the runtime. Mounting the real runtime opens a WebSocket,
// so the runtime is replaced by a probe that reports the prop it received.

import { describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/react";

vi.mock("./AcpRuntime", () => ({
  SUBAGENT_TASK_NAME: "Task",
  TODO_GROUP_NAME: "Todos",
  TOOL_GROUP_NAME: "Tools",
  AcpRuntime: ({ active }: { active?: boolean }) => <div data-testid="probe">{String(active)}</div>,
}));

import { StructuredView } from "./StructuredView";

describe("StructuredView active plumbing", () => {
  it("forwards a suspended view to the runtime", () => {
    const { getByTestId } = render(
      <StructuredView
        sessionId="s1"
        acpWorkerState="running"
        tool="claude"
        acpAgent={null}
        archivedAt={null}
        snoozedUntil={null}
        trashedAt={null}
        active={false}
      />,
    );
    expect(getByTestId("probe").textContent).toBe("false");
  });

  it("is active by default, for callers that never suspend", () => {
    const { getByTestId } = render(
      <StructuredView
        sessionId="s1"
        acpWorkerState="running"
        tool="claude"
        acpAgent={null}
        archivedAt={null}
        snoozedUntil={null}
        trashedAt={null}
      />,
    );
    expect(getByTestId("probe").textContent).toBe("true");
  });
});
