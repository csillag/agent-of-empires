// @vitest-environment jsdom
//
// The desktop keep-alive host (#live-session-tabs): switching sessions flips
// which layer is visible instead of remounting one. The real view opens a
// WebSocket, so it is stubbed down to a probe that reports its id and its
// active flag.

import { Suspense, useEffect } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";

import type { SessionResponse } from "../../lib/types";

// Sessions whose view has not finished loading. Rendering one throws its
// promise, the way a lazy chunk or a `use()` does, until the test releases it.
const loading = new Map<string, Promise<void>>();
// Session ids whose probe has a live effect, so a test can tell a view that
// stayed mounted from one React tore down to show a fallback.
const live = new Set<string>();

function holdView(sessionId: string): () => void {
  let release = () => {};
  const pending = new Promise<void>((resolve) => {
    release = () => {
      loading.delete(sessionId);
      resolve();
    };
  });
  loading.set(sessionId, pending);
  return release;
}

function Probe({ sessionId, active }: { sessionId: string; active?: boolean }) {
  useEffect(() => {
    live.add(sessionId);
    return () => {
      live.delete(sessionId);
    };
  }, [sessionId]);
  return <div data-testid={`view-${sessionId}`} data-active={String(active)} />;
}

vi.mock("../acp/StructuredView", () => ({
  StructuredView: ({ sessionId, active }: { sessionId: string; active?: boolean }) => {
    const pending = loading.get(sessionId);
    if (pending) throw pending;
    return <Probe sessionId={sessionId} active={active} />;
  },
}));

afterEach(() => {
  loading.clear();
  live.clear();
});

import { SessionViewHost } from "../SessionViewHost";

function session(id: string, overrides: Partial<SessionResponse> = {}): SessionResponse {
  return {
    id,
    title: id,
    project_path: "/tmp/t",
    group_path: "/tmp",
    tool: "claude",
    status: "Running",
    yolo_mode: false,
    created_at: new Date().toISOString(),
    last_accessed_at: null,
    last_error: null,
    branch: null,
    main_repo_path: null,
    is_sandboxed: false,
    has_terminal: true,
    profile: "default",
    workspace_repos: [],
    view: "structured",
    ...overrides,
  } as SessionResponse;
}

function mount(activeSessionId: string, sessions: SessionResponse[]) {
  return render(
    <Suspense fallback={null}>
      <SessionViewHost
        activeSessionId={activeSessionId}
        sessions={sessions}
        onOpenFileRef={() => {}}
        onOpenAgentsPane={() => {}}
        onRestoreSession={() => {}}
      />
    </Suspense>,
  );
}

function layerOf(id: string): HTMLElement {
  return screen.getByTestId(`view-${id}`).parentElement as HTMLElement;
}

describe("SessionViewHost", () => {
  it("keeps the previous session mounted, hidden and inert after a switch", async () => {
    const sessions = [session("a"), session("b")];
    const { rerender } = mount("a", sessions);
    await screen.findByTestId("view-a");

    rerender(
      <Suspense fallback={null}>
        <SessionViewHost
          activeSessionId="b"
          sessions={sessions}
          onOpenFileRef={() => {}}
          onOpenAgentsPane={() => {}}
          onRestoreSession={() => {}}
        />
      </Suspense>,
    );
    await screen.findByTestId("view-b");

    expect(screen.getByTestId("view-a").dataset.active).toBe("false");
    expect(screen.getByTestId("view-b").dataset.active).toBe("true");
    expect(layerOf("a").hasAttribute("hidden")).toBe(true);
    expect(layerOf("a").hasAttribute("inert")).toBe(true);
    expect(layerOf("b").hasAttribute("hidden")).toBe(false);
  });

  it("renders layers in a stable order so a promotion does not move a node", async () => {
    const sessions = [session("a"), session("b")];
    const { container, rerender } = mount("a", sessions);
    await screen.findByTestId("view-a");
    rerender(
      <Suspense fallback={null}>
        <SessionViewHost
          activeSessionId="b"
          sessions={sessions}
          onOpenFileRef={() => {}}
          onOpenAgentsPane={() => {}}
          onRestoreSession={() => {}}
        />
      </Suspense>,
    );
    await screen.findByTestId("view-b");
    const order = [...container.querySelectorAll("[data-session-view]")].map(
      (el) => (el as HTMLElement).dataset.sessionView,
    );
    expect(order).toEqual(["a", "b"]);
  });

  it("unmounts the least recently visited view past the cap of eight", async () => {
    const ids = ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9"];
    const sessions = ids.map((id) => session(id));
    const { rerender } = mount("s1", sessions);
    await screen.findByTestId("view-s1");
    for (const id of ids.slice(1)) {
      rerender(
        <Suspense fallback={null}>
          <SessionViewHost
            activeSessionId={id}
            sessions={sessions}
            onOpenFileRef={() => {}}
            onOpenAgentsPane={() => {}}
            onRestoreSession={() => {}}
          />
        </Suspense>,
      );
      await screen.findByTestId(`view-${id}`);
    }
    expect(screen.queryByTestId("view-s1")).toBeNull();
    expect(screen.getByTestId("view-s2")).toBeDefined();
    expect(screen.getByTestId("view-s9").dataset.active).toBe("true");
  });

  it("drops a kept view whose session was deleted, archived or trashed", async () => {
    const all = [session("a"), session("b"), session("c"), session("d")];
    const { rerender } = mount("a", all);
    await screen.findByTestId("view-a");
    for (const id of ["b", "c", "d"]) {
      rerender(
        <Suspense fallback={null}>
          <SessionViewHost
            activeSessionId={id}
            sessions={all}
            onOpenFileRef={() => {}}
            onOpenAgentsPane={() => {}}
            onRestoreSession={() => {}}
          />
        </Suspense>,
      );
      await screen.findByTestId(`view-${id}`);
    }

    const after = [
      // "a" deleted, "b" archived, "c" trashed; "d" is still on screen.
      session("b", { archived_at: "2026-09-20T10:00:00Z" }),
      session("c", { trashed_at: "2026-09-20T10:00:00Z" }),
      session("d"),
    ];
    rerender(
      <Suspense fallback={null}>
        <SessionViewHost
          activeSessionId="d"
          sessions={after}
          onOpenFileRef={() => {}}
          onOpenAgentsPane={() => {}}
          onRestoreSession={() => {}}
        />
      </Suspense>,
    );
    await screen.findByTestId("view-d");

    expect(screen.queryByTestId("view-a")).toBeNull();
    expect(screen.queryByTestId("view-b")).toBeNull();
    expect(screen.queryByTestId("view-c")).toBeNull();
    expect(screen.getByTestId("view-d").dataset.active).toBe("true");
  });

  it("keeps the session on screen even when it is trashed", async () => {
    mount("a", [session("a", { trashed_at: "2026-09-20T10:00:00Z" })]);
    expect((await screen.findByTestId("view-a")).dataset.active).toBe("true");
  });

  it("keeps the visible view mounted while a newly added layer is still loading", async () => {
    const sessions = [session("a"), session("b")];
    const { rerender } = mount("a", sessions);
    await screen.findByTestId("view-a");
    expect(live.has("a")).toBe(true);

    const release = holdView("b");
    rerender(
      <Suspense fallback={null}>
        <SessionViewHost
          activeSessionId="b"
          sessions={sessions}
          onOpenFileRef={() => {}}
          onOpenAgentsPane={() => {}}
          onRestoreSession={() => {}}
        />
      </Suspense>,
    );

    // A layer that suspends must not take the layer on screen with it. React
    // keeps a hidden boundary's tree mounted, so the effects survive either
    // way; the display check is what catches the view going blank.
    expect(live.has("a")).toBe(true);
    expect(layerOf("a").style.display).not.toBe("none");

    await act(async () => {
      release();
    });
    expect(await screen.findByTestId("view-b")).toBeDefined();
    expect(live.has("a")).toBe(true);
  });
});
